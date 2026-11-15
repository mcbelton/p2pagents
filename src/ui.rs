//! Local web UI (`p2pagents --name me --ui`): a single-page app served on
//! 127.0.0.1 only, guarded by a random access token and a Host check, with live
//! updates over Server-Sent Events.

use std::collections::{HashMap, VecDeque};
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
use tokio::task::AbortHandle;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::agent::{Agent, Event, Network};
use crate::audit::{AuditLog, Outcome};
use crate::llm::{Access, Executor, LlmTask, Provider, TaskContext};
use crate::plan::{Plan, SquadStore, StepState, run_plan};
use crate::profile::{Profile, validate_name};
use crate::runs::RunRecord;
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
    /// For plan steps: the plan, the step id and the steps it waits for.
    #[serde(skip_serializing_if = "Option::is_none")]
    plan: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    step: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    after: Vec<String>,
    /// An approval step (a person signs off) rather than an LLM task.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    approval: bool,
    /// For peers' results: "verified" (signed by the peer for this output) or "unverified".
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence: Option<&'static str>,
}

impl TaskView {
    fn finished(&self) -> bool {
        !matches!(self.status, "running" | "waiting")
    }
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
    /// Abort handles of running tasks (a plan's steps share one), for `DELETE /api/tasks/{id}`.
    running: Mutex<HashMap<u64, AbortHandle>>,
    next_task_id: AtomicU64,
    squads: Mutex<SquadStore>,
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
                    "owner": p.owner,
                    "role": p.role,
                    "shares": p.shares,
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
                    "access": t.access,
                    "shares": t.shares,
                    "connected": peers.iter().any(|p| p.id == t.id),
                })
            })
            .collect();
        let squads: Vec<Value> = self
            .squads
            .lock()
            .unwrap()
            .squads
            .iter()
            .map(|s| {
                let members: Vec<Value> = s
                    .members
                    .iter()
                    .map(|m| json!({ "name": m, "online": peers.iter().any(|p| &p.name == m) }))
                    .collect();
                json!({ "name": s.name, "members": members })
            })
            .collect();
        json!({ "peers": peers_json, "trusted": trusted, "squads": squads })
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
            "shares": if self.info.serving { json!(self.agent.shares().shares) } else { json!([]) },
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
    let squads = SquadStore::load(&info.profile.squads_path()).unwrap_or_default();
    let shared = Arc::new(Shared {
        agent,
        info,
        executor,
        token: token.clone(),
        port: AtomicU16::new(0),
        feed: Mutex::new(VecDeque::new()),
        next_feed_id: AtomicU64::new(1),
        tasks: Mutex::new(Vec::new()),
        squads: Mutex::new(squads),
        running: Mutex::new(HashMap::new()),
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
                Event::TaskCompleted {
                    from,
                    task_id,
                    outcome,
                } => {
                    let (level, verb) = match outcome {
                        Outcome::Done => ("task", "finished"),
                        Outcome::Cancelled => ("warn", "was cancelled"),
                        _ => ("warn", "failed"),
                    };
                    pump.system(level, format!("Task #{task_id} from {from} {verb}"));
                    pump.send(json!({ "type": "audit" }));
                }
                Event::TaskRefused {
                    from,
                    task_id,
                    reason,
                } => {
                    pump.system(
                        "warn",
                        format!("Refused task #{task_id} from {from}: {reason}"),
                    );
                    pump.send(json!({ "type": "audit" }));
                }
                Event::ApprovalRequested { id, from, title } => {
                    pump.system(
                        "task",
                        format!("{from} asks you to approve: {title} (#{id})"),
                    );
                    pump.send(json!({ "type": "approvals", "approvals": approvals_json(&pump) }));
                }
                Event::ApprovalClosed { .. } => {
                    pump.send(json!({ "type": "approvals", "approvals": approvals_json(&pump) }));
                }
            }
        }
    });

    let api = Router::new()
        .route("/api/state", get(state))
        .route("/api/events", get(events_stream))
        .route("/api/chat", post(chat))
        .route("/api/tasks", post(create_task))
        .route("/api/tasks/{id}", delete(cancel_task))
        .route("/api/audit", get(audit))
        .route("/api/plans", post(create_plan))
        .route("/api/runs", get(list_runs))
        .route("/api/runs/{id}", get(get_run))
        .route("/api/approvals/{id}", post(answer_approval))
        .route("/api/squads", post(save_squad))
        .route("/api/squads/{name}", delete(remove_squad))
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
    v["approvals"] = approvals_json(&s);
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
    /// For peers: the shared folder to run in (default: their `data` folder).
    share: Option<String>,
}

async fn create_task(
    State(s): State<AppState>,
    Json(body): Json<TaskBody>,
) -> Result<Json<Value>, ApiError> {
    let prompt = body.prompt.trim().to_owned();
    if prompt.is_empty() {
        return Err(bad_request("prompt is empty"));
    }
    let mut task = LlmTask::new(body.provider, prompt.clone());
    if let Some(share) = body
        .share
        .filter(|s| !s.is_empty() && body.target != "local")
    {
        task = task.in_share(share);
    }
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
        plan: None,
        step: None,
        after: Vec::new(),
        approval: false,
        evidence: None,
    };
    s.tasks.lock().unwrap().push(view.clone());
    s.send(json!({ "type": "task", "task": view }));

    let shared = s.clone();
    let handle = tokio::spawn(async move {
        let mut evidence = None;
        let result = match route {
            Route::Local(executor) => {
                let ctx = TaskContext {
                    access: shared.info.access,
                    workspace: shared.info.workspace.clone(),
                };
                executor.execute(&task, &ctx).await
            }
            Route::Peer(peer, _) => match shared.agent.delegate_verified(peer, task).await {
                Ok(d) => {
                    if d.evidence.is_some() {
                        evidence = Some(if d.verified { "verified" } else { "unverified" });
                    }
                    if d.ok {
                        Ok(d.output)
                    } else {
                        Err(anyhow::anyhow!("{}", d.output))
                    }
                }
                Err(e) => Err(e),
            },
        };
        shared.running.lock().unwrap().remove(&id);
        shared.update_task(id, |t| {
            t.finished_ms = Some(now_ms());
            t.evidence = evidence;
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
    {
        // The task removes itself when done; insert only if it's still running.
        let mut running = s.running.lock().unwrap();
        if !handle.is_finished() {
            running.insert(id, handle.abort_handle());
        }
    }
    Ok(Json(json!(view)))
}

/// Cancels a running task, or the whole plan a step belongs to. Dropping a
/// delegation tells the peer to stop it; a local task's provider run is aborted.
async fn cancel_task(
    State(s): State<AppState>,
    Path(id): Path<u64>,
) -> Result<Json<Value>, ApiError> {
    let handle = s.running.lock().unwrap().remove(&id).ok_or_else(|| {
        (
            StatusCode::CONFLICT,
            Json(json!({ "error": "that task is not running" })),
        )
    })?;
    handle.abort();
    let plan = s
        .tasks
        .lock()
        .unwrap()
        .iter()
        .find(|t| t.id == id)
        .and_then(|t| t.plan);
    let ids: Vec<u64> = match plan {
        Some(plan) => s
            .tasks
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.plan == Some(plan) && !t.finished())
            .map(|t| t.id)
            .collect(),
        None => vec![id],
    };
    for id in ids {
        s.running.lock().unwrap().remove(&id);
        s.update_task(id, |t| {
            t.finished_ms = Some(now_ms());
            t.status = "cancelled";
            t.output = Some("Cancelled by you.".into());
        });
    }
    Ok(Json(json!({ "cancelled": id })))
}

#[derive(Deserialize)]
struct PlanBody {
    text: String,
}

/// Runs a plan (see [`crate::plan`]); each step shows up as a task, updated live.
/// When it ends, the run and its evidence report are saved in the profile.
async fn create_plan(
    State(s): State<AppState>,
    Json(body): Json<PlanBody>,
) -> Result<Json<Value>, ApiError> {
    let plan = Plan::parse(&body.text).map_err(|e| bad_request(format!("{e:#}")))?;
    let plan_id = s.next_task_id.fetch_add(1, Ordering::Relaxed);
    let mut ids = HashMap::new();
    for step in &plan.steps {
        let id = s.next_task_id.fetch_add(1, Ordering::Relaxed);
        ids.insert(step.id.clone(), id);
        let view = TaskView {
            id,
            provider: step.provider,
            prompt: step.prompt.clone(),
            target: format!(
                "{}{}",
                step.target,
                step.share
                    .as_ref()
                    .map(|f| format!(":{f}"))
                    .unwrap_or_default()
            ),
            status: "waiting",
            output: None,
            started_ms: now_ms(),
            finished_ms: None,
            plan: Some(plan_id),
            step: Some(step.id.clone()),
            after: step.after.clone(),
            approval: step.approve,
            evidence: None,
        };
        s.tasks.lock().unwrap().push(view.clone());
        s.send(json!({ "type": "task", "task": view }));
    }
    let squads = s.squads.lock().unwrap().clone();
    let shared = s.clone();
    let step_ids = ids.clone();
    let text = body.text.clone();
    let handle = tokio::spawn(async move {
        let started = crate::runs::unix_now();
        let states = run_plan(&shared.agent.clone(), &squads, &plan, |step, state| {
            let id = step_ids[step];
            if !matches!(state, StepState::Waiting | StepState::Running { .. }) {
                shared.running.lock().unwrap().remove(&id);
            }
            shared.update_task(id, |t| match state {
                StepState::Waiting => {}
                StepState::Running { peer } => {
                    t.status = "running";
                    t.target = peer.clone();
                    t.started_ms = now_ms();
                }
                StepState::Done {
                    peer,
                    output,
                    evidence,
                    verified,
                    ..
                } => {
                    t.status = "done";
                    t.target = peer.clone();
                    t.output = Some(output.clone());
                    t.finished_ms = Some(now_ms());
                    t.evidence = evidence
                        .as_ref()
                        .map(|_| if *verified { "verified" } else { "unverified" });
                }
                StepState::Failed {
                    error,
                    evidence,
                    verified,
                    ..
                } => {
                    t.status = "failed";
                    t.output = Some(error.clone());
                    t.finished_ms = Some(now_ms());
                    t.evidence = evidence
                        .as_ref()
                        .map(|_| if *verified { "verified" } else { "unverified" });
                }
                StepState::Skipped { reason } => {
                    t.status = "skipped";
                    t.output = Some(format!("Skipped: {reason}"));
                    t.finished_ms = Some(now_ms());
                }
            });
        })
        .await;
        let mut run = RunRecord::new(
            shared.agent.name(),
            &shared.agent.id().to_string(),
            &text,
            &plan,
            &states,
            started,
        );
        run.peer_ids = crate::runs::peer_ids(&shared.agent);
        match run.save(&shared.info.profile.runs_dir()) {
            Ok(_) => shared.system(
                if run.passed() { "info" } else { "warn" },
                format!("Evidence run {}: {}", run.id, run.verdict_line()),
            ),
            Err(e) => shared.system("warn", format!("Could not save the run: {e:#}")),
        }
        shared.send(json!({ "type": "runs" }));
    });
    {
        let mut running = s.running.lock().unwrap();
        let tasks = s.tasks.lock().unwrap();
        for id in ids.values() {
            if tasks.iter().any(|t| t.id == *id && !t.finished()) {
                running.insert(*id, handle.abort_handle());
            }
        }
    }
    s.system("task", format!("Plan started: {} steps", ids.len()));
    Ok(Json(json!({ "plan": plan_id, "steps": ids })))
}

#[derive(Deserialize)]
struct SquadBody {
    name: String,
    members: Vec<String>,
}

async fn save_squad(
    State(s): State<AppState>,
    Json(body): Json<SquadBody>,
) -> Result<Json<Value>, ApiError> {
    let trusted = s.agent.trusted();
    if let Some(unknown) = body
        .members
        .iter()
        .find(|m| trusted.get_by_name(m).is_none())
    {
        return Err(bad_request(format!("'{unknown}' is not on the allowlist")));
    }
    let saved = {
        let mut squads = s.squads.lock().unwrap();
        squads
            .set(body.name.trim(), body.members)
            .map_err(|e| bad_request(format!("{e:#}")))?;
        squads.save(&s.info.profile.squads_path())
    };
    saved.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("{e:#}") })),
        )
    })?;
    s.system("info", format!("Squad {} saved", body.name.trim()));
    s.broadcast_peers();
    Ok(Json(json!({ "name": body.name.trim() })))
}

async fn remove_squad(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let saved = {
        let mut squads = s.squads.lock().unwrap();
        if !squads.remove(&name) {
            return Err(bad_request(format!("there is no squad {name}")));
        }
        squads.save(&s.info.profile.squads_path())
    };
    saved.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("{e:#}") })),
        )
    })?;
    s.broadcast_peers();
    Ok(Json(json!({ "removed": name })))
}

#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<usize>,
}

/// Saved evidence runs, newest first.
async fn list_runs(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let runs = RunRecord::list(&s.info.profile.runs_dir()).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("{e:#}") })),
        )
    })?;
    Ok(Json(json!({ "runs": runs })))
}

/// One run: its summary, the Markdown report and a fresh signature check.
async fn get_run(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let dir = s.info.profile.runs_dir();
    let run = RunRecord::load(&dir, &id).map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("{e:#}") })),
        )
    })?;
    let (run, checks) = run.reverified(Some(&dir.join(&run.id)));
    let checks: Vec<Value> = checks
        .into_iter()
        .map(|(step, r)| match r {
            Ok(text) => json!({ "step": step, "ok": true, "text": text }),
            Err(e) => json!({ "step": step, "ok": false, "text": format!("{e:#}") }),
        })
        .collect();
    Ok(Json(json!({
        "summary": run.summary(),
        "verdict": run.verdict_line(),
        "report": run.render_markdown(),
        "signatures": checks,
    })))
}

fn approvals_json(s: &Shared) -> Value {
    json!(s.agent.approvals())
}

#[derive(Deserialize)]
struct ApprovalBody {
    approved: bool,
    #[serde(default)]
    note: String,
}

async fn answer_approval(
    State(s): State<AppState>,
    Path(id): Path<u64>,
    Json(body): Json<ApprovalBody>,
) -> Result<Json<Value>, ApiError> {
    if !body.approved && body.note.trim().is_empty() {
        return Err(bad_request("say why you reject it"));
    }
    s.agent
        .answer_approval(id, body.approved, &body.note)
        .await
        .map_err(|e| bad_request(format!("{e:#}")))?;
    s.system(
        "info",
        format!(
            "You {} request #{id}",
            if body.approved {
                "approved"
            } else {
                "rejected"
            }
        ),
    );
    Ok(Json(json!({ "answered": id })))
}

/// The audit log of tasks peers ran on this agent, newest first.
async fn audit(
    State(s): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Value>, ApiError> {
    if !s.info.serving {
        return Ok(Json(json!({ "enabled": false, "entries": [] })));
    }
    let path = s.info.profile.audit_path();
    let mut entries = AuditLog::read(&path, q.limit.unwrap_or(100).min(1000)).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("reading the audit log: {e}") })),
        )
    })?;
    entries.reverse();
    Ok(Json(json!({ "enabled": true, "entries": entries })))
}

#[derive(Deserialize)]
struct AllowBody {
    ticket: String,
    name: Option<String>,
    #[serde(default)]
    delegate: bool,
    tasks_per_hour: Option<u32>,
    #[serde(default)]
    access: Access,
    /// Folders the peer's tasks may use (default: the `data` share).
    shares: Option<Vec<String>>,
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
    let shares = body
        .shares
        .unwrap_or_else(|| vec![crate::shares::DATA_SHARE.to_owned()]);
    // Folder grants only matter for peers that may delegate tasks.
    let known = s.agent.shares();
    if body.delegate
        && let Some(missing) = shares.iter().find(|n| known.get(n).is_none())
    {
        return Err(bad_request(format!(
            "this agent shares no folder named '{missing}'"
        )));
    }
    s.agent.allow(TrustedPeer {
        name: name.clone(),
        id: ticket.addr.id,
        addrs: ticket.addr.addrs,
        can_delegate: body.delegate,
        tasks_per_hour: body
            .tasks_per_hour
            .unwrap_or(crate::trust::DEFAULT_TASKS_PER_HOUR),
        access: if body.delegate {
            body.access
        } else {
            Access::ReadOnly
        },
        shares: shares.clone(),
    });
    s.save_trust()?;
    s.system(
        "info",
        format!(
            "Allowlisted {name}{}",
            if body.delegate {
                format!(
                    " (may delegate tasks, {} access, folders: {})",
                    body.access,
                    shares.join(", ")
                )
            } else {
                String::new()
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
