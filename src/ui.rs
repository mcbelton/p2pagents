//! Local web UI (`p2pagents --name me --ui`): a single-page app served on
//! 127.0.0.1 only, guarded by a random access token and a Host check, with live
//! updates over Server-Sent Events.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::agent::{Agent, Event, Network};
use crate::llm::{Access, Executor, LlmTask, Provider};
use crate::profile::{Profile, validate_name};
use crate::trust::{Ticket, TrustedPeer};

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_CSS: &str = include_str!("../ui/app.css");
const APP_JS: &str = include_str!("../ui/app.js");
const FEED_LIMIT: usize = 500;

/// What the UI shows about this agent besides live state.
pub struct UiInfo {
    pub profile: Profile,
    pub network: Network,
    pub serving: bool,
    pub access: Access,
    pub workspace: PathBuf,
    pub providers: Vec<Provider>,
    pub accounts: Vec<(Provider, String)>,
}

#[derive(Debug, Clone, Serialize)]
struct TaskView {
    id: u64,
    provider: Provider,
    prompt: String,
    target: String,
    status: &'static str,
    output: Option<String>,
    started_ms: u128,
    finished_ms: Option<u128>,
}

struct Shared {
    agent: Agent,
    info: UiInfo,
    executor: Option<Arc<dyn Executor>>,
    token: String,
    port: AtomicU16,
    feed: Mutex<VecDeque<Value>>,
    next_feed_id: AtomicU64,
    tasks: Mutex<Vec<TaskView>>,
    next_task_id: AtomicU64,
    tx: broadcast::Sender<String>,
}

type AppState = Arc<Shared>;
type ApiError = (StatusCode, Json<Value>);

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn bad_request(msg: impl ToString) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": msg.to_string() })),
    )
}

impl Shared {
    fn send(&self, msg: Value) {
        let _ = self.tx.send(msg.to_string());
    }

    fn push_feed(&self, mut item: Value) {
        item["id"] = json!(self.next_feed_id.fetch_add(1, Ordering::Relaxed));
        item["ts"] = json!(now_ms());
        {
            let mut feed = self.feed.lock().unwrap();
            feed.push_back(item.clone());
            while feed.len() > FEED_LIMIT {
                feed.pop_front();
            }
        }
        self.send(json!({ "type": "feed", "item": item }));
    }

    fn system(&self, level: &str, text: impl Into<String>) {
        self.push_feed(json!({ "kind": "system", "level": level, "text": text.into() }));
    }

    fn peers_json(&self) -> Value {
        let peers = self.agent.peers();
        let peers_json: Vec<Value> = peers
            .iter()
            .map(|p| {
                json!({
                    "id": p.id.to_string(),
                    "short": p.id.fmt_short().to_string(),
                    "name": p.name,
                    "capabilities": p.capabilities,
                })
            })
            .collect();
        let trusted: Vec<Value> = self
            .agent
            .trusted()
            .peers
            .iter()
            .map(|t| {
                json!({
                    "id": t.id.to_string(),
                    "short": t.id.fmt_short().to_string(),
                    "name": t.name,
                    "can_delegate": t.can_delegate,
                    "tasks_per_hour": t.tasks_per_hour,
                    "connected": peers.iter().any(|p| p.id == t.id),
                })
            })
            .collect();
        json!({ "peers": peers_json, "trusted": trusted })
    }

    fn broadcast_peers(&self) {
        let mut msg = self.peers_json();
        msg["type"] = json!("peers");
        self.send(msg);
    }

    fn me_json(&self) -> Value {
        let addr = self.agent.addr();
        let ticket = Ticket {
            name: self.info.profile.name.clone(),
            addr: addr.clone(),
        }
        .encode();
        let accounts: Vec<Value> = self
            .info
            .accounts
            .iter()
            .map(|(p, account)| json!({ "provider": p, "account": account }))
            .collect();
        json!({
            "name": self.agent.name(),
            "id": self.agent.id().to_string(),
            "short": self.agent.id().fmt_short().to_string(),
            "profile": self.info.profile.name,
            "network": match self.info.network { Network::Internet => "internet", Network::Local => "local" },
            "relay": addr.relay_urls().next().map(|u| u.to_string()),
            "serving": self.info.serving,
            "access": self.info.access,
            "workspace": self.info.workspace.display().to_string(),
            "providers": self.info.providers,
            "accounts": accounts,
            "ticket": ticket,
        })
    }

    fn update_task(&self, id: u64, f: impl FnOnce(&mut TaskView)) {
        let task = {
            let mut tasks = self.tasks.lock().unwrap();
            let Some(task) = tasks.iter_mut().find(|t| t.id == id) else {
                return;
            };
            f(task);
            task.clone()
        };
        self.send(json!({ "type": "task", "task": task }));
    }

    fn save_trust(&self) -> Result<(), ApiError> {
        self.agent
            .trusted()
            .save(&self.info.profile.trust_path())
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("{e:#}") })),
                )
            })
    }
}

/// A running UI server.
pub struct UiServer {
    pub url: String,
}

/// Starts the UI on `bind` (use 127.0.0.1). Consumes the agent's event stream.
pub async fn start(
    agent: Agent,
    mut events: mpsc::UnboundedReceiver<Event>,
    info: UiInfo,
    executor: Option<Arc<dyn Executor>>,
    bind: SocketAddr,
) -> Result<UiServer> {
    let token = data_encoding::HEXLOWER.encode(&iroh::SecretKey::generate().to_bytes()[..16]);
    let (tx, _) = broadcast::channel(256);
    let shared = Arc::new(Shared {
        agent,
        info,
        executor,
        token: token.clone(),
        port: AtomicU16::new(0),
        feed: Mutex::new(VecDeque::new()),
        next_feed_id: AtomicU64::new(1),
        tasks: Mutex::new(Vec::new()),
        next_task_id: AtomicU64::new(1),
        tx,
    });

    let pump = shared.clone();
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                Event::PeerConnected { name, .. } => {
                    pump.system("join", format!("{name} connected"));
                    pump.broadcast_peers();
                }
                Event::PeerDisconnected { name, .. } => {
                    pump.system("leave", format!("{name} disconnected"));
                    pump.broadcast_peers();
                }
                Event::PeerRejected { id } => pump.system(
                    "warn",
                    format!(
                        "Refused a connection from {} (not on the allowlist)",
                        id.fmt_short()
                    ),
                ),
                Event::Chat { from, text } => pump.push_feed(
                    json!({ "kind": "chat", "from": from, "text": text, "self": false }),
                ),
                Event::TaskReceived {
                    from,
                    task_id,
                    provider,
                } => pump.system(
                    "task",
                    format!("{from} delegated task #{task_id} ({provider}) to this agent"),
                ),
                Event::TaskCompleted { from, task_id, ok } => pump.system(
                    if ok { "task" } else { "warn" },
                    format!(
                        "Task #{task_id} from {from} {}",
                        if ok { "finished" } else { "failed" }
                    ),
                ),
            }
        }
    });

    let api = Router::new()
        .route("/api/state", get(state))
        .route("/api/events", get(events_stream))
        .route("/api/chat", post(chat))
        .route("/api/tasks", post(create_task))
        .route("/api/trust", post(allow_peer))
        .route("/api/trust/{id}", delete(remove_peer))
        .route_layer(middleware::from_fn_with_state(
            shared.clone(),
            require_token,
        ));
    let app = Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX_HTML) }),
        )
        .route(
            "/app.css",
            get(|| async { asset("text/css; charset=utf-8", APP_CSS) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .merge(api)
        .layer(middleware::from_fn_with_state(
            shared.clone(),
            require_local_host,
        ))
        .with_state(shared.clone());

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .context("binding the UI server")?;
    let port = listener.local_addr()?.port();
    shared.port.store(port, Ordering::Relaxed);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    shared.system("info", "Agent started");
    Ok(UiServer {
        url: format!("http://127.0.0.1:{port}/#t={token}"),
    })
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; style-src 'self'; script-src 'self'; img-src 'self' data:; frame-ancestors 'none'",
            ),
        ],
        body,
    )
        .into_response()
}

/// Rejects requests whose Host isn't this loopback server (DNS-rebinding defense).
async fn require_local_host(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let port = s.port.load(Ordering::Relaxed);
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}") {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "forbidden host").into_response()
    }
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// API calls need the access token (header, or `?token=` for EventSource).
async fn require_token(
    State(s): State<AppState>,
    Query(q): Query<TokenQuery>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if bearer == Some(s.token.as_str()) || q.token.as_deref() == Some(s.token.as_str()) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "missing or wrong access token" })),
        )
            .into_response()
    }
}

async fn state(State(s): State<AppState>) -> Json<Value> {
    let mut v = s.peers_json();
    v["me"] = s.me_json();
    v["feed"] = json!(s.feed.lock().unwrap().iter().cloned().collect::<Vec<_>>());
    v["tasks"] = json!(s.tasks.lock().unwrap().clone());
    Json(v)
}

async fn events_stream(
    State(s): State<AppState>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let stream = BroadcastStream::new(s.tx.subscribe())
        .filter_map(|msg| msg.ok())
        .map(|msg| Ok(SseEvent::default().data(msg)));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
struct ChatBody {
    text: String,
}

async fn chat(
    State(s): State<AppState>,
    Json(body): Json<ChatBody>,
) -> Result<Json<Value>, ApiError> {
    let text = body.text.trim();
    if text.is_empty() {
        return Err(bad_request("message is empty"));
    }
    let sent = s.agent.broadcast(text);
    s.push_feed(json!({ "kind": "chat", "from": s.agent.name(), "text": text, "self": true }));
    Ok(Json(json!({ "sent": sent })))
}

#[derive(Deserialize)]
struct TaskBody {
    provider: Provider,
    prompt: String,
    /// "local", "auto" (first peer serving the provider), or a peer endpoint id.
    target: String,
}

async fn create_task(
    State(s): State<AppState>,
    Json(body): Json<TaskBody>,
) -> Result<Json<Value>, ApiError> {
    let prompt = body.prompt.trim().to_owned();
    if prompt.is_empty() {
        return Err(bad_request("prompt is empty"));
    }
    let task = LlmTask::new(body.provider, prompt.clone());
    enum Route {
        Local(Arc<dyn Executor>),
        Peer(iroh::EndpointId, String),
    }
    let route = match body.target.as_str() {
        "local" => match &s.executor {
            Some(e) if e.providers().contains(&body.provider) => Route::Local(e.clone()),
            _ => {
                return Err(bad_request(format!(
                    "{} is not enabled on this agent",
                    body.provider
                )));
            }
        },
        "auto" => {
            let peer = s.agent.find_peer(body.provider).ok_or_else(|| {
                bad_request(format!("no connected peer serves {}", body.provider))
            })?;
            Route::Peer(peer.id, peer.name)
        }
        id => {
            let peer = s
                .agent
                .peers()
                .into_iter()
                .find(|p| p.id.to_string() == id)
                .ok_or_else(|| bad_request("that peer is not connected"))?;
            Route::Peer(peer.id, peer.name)
        }
    };
    let id = s.next_task_id.fetch_add(1, Ordering::Relaxed);
    let target = match &route {
        Route::Local(_) => "this agent".to_owned(),
        Route::Peer(_, name) => name.clone(),
    };
    let view = TaskView {
        id,
        provider: body.provider,
        prompt,
        target: target.clone(),
        status: "running",
        output: None,
        started_ms: now_ms(),
        finished_ms: None,
    };
    s.tasks.lock().unwrap().push(view.clone());
    s.send(json!({ "type": "task", "task": view }));

    let shared = s.clone();
    tokio::spawn(async move {
        let result = match route {
            Route::Local(executor) => executor.execute(&task).await,
            Route::Peer(peer, _) => shared.agent.delegate(peer, task).await,
        };
        shared.update_task(id, |t| {
            t.finished_ms = Some(now_ms());
            match result {
                Ok(out) => {
                    t.status = "done";
                    t.output = Some(out);
                }
                Err(e) => {
                    t.status = "failed";
                    t.output = Some(format!("{e:#}"));
                }
            }
        });
    });
    Ok(Json(json!(view)))
}

#[derive(Deserialize)]
struct AllowBody {
    ticket: String,
    name: Option<String>,
    #[serde(default)]
    delegate: bool,
    tasks_per_hour: Option<u32>,
}

async fn allow_peer(
    State(s): State<AppState>,
    Json(body): Json<AllowBody>,
) -> Result<Json<Value>, ApiError> {
    let ticket = Ticket::parse(&body.ticket).map_err(bad_request)?;
    let name = body
        .name
        .map(|n| n.trim().to_owned())
        .filter(|n| !n.is_empty())
        .or_else(|| Some(ticket.name.clone()).filter(|n| !n.is_empty()))
        .ok_or_else(|| bad_request("a bare endpoint id has no name; enter a name"))?;
    validate_name(&name)
        .map_err(|_| bad_request("name may only use letters, digits, '-', '_' and '.'"))?;
    if ticket.addr.id == s.agent.id() {
        return Err(bad_request("that is this agent's own ticket"));
    }
    s.agent.allow(TrustedPeer {
        name: name.clone(),
        id: ticket.addr.id,
        addrs: ticket.addr.addrs,
        can_delegate: body.delegate,
        tasks_per_hour: body
            .tasks_per_hour
            .unwrap_or(crate::trust::DEFAULT_TASKS_PER_HOUR),
    });
    s.save_trust()?;
    s.system(
        "info",
        format!(
            "Allowlisted {name}{}",
            if body.delegate {
                " (may delegate tasks)"
            } else {
                ""
            }
        ),
    );
    s.broadcast_peers();
    Ok(Json(json!({ "name": name })))
}

async fn remove_peer(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let name = s
        .agent
        .trusted()
        .peers
        .iter()
        .find(|p| p.id.to_string() == id)
        .map(|p| p.name.clone());
    if !s.agent.remove_trusted(&id) {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "not on the allowlist" })),
        ));
    }
    s.save_trust()?;
    s.system(
        "info",
        format!("Removed {} from the allowlist", name.unwrap_or(id)),
    );
    s.broadcast_peers();
    Ok(Json(json!({ "removed": true })))
}
