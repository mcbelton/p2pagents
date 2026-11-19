//! Local web UI (`p2pagents --name me --ui`): a single-page app served on
//! 127.0.0.1 only, guarded by a random access token and a Host check, with live
//! updates over Server-Sent Events.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
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

mod tools;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_CSS: &str = include_str!("../ui/app.css");
const APP_JS: &str = include_str!("../ui/app.js");
const TOOLS_JS: &str = include_str!("../ui/tools.js");
const PLAN_TEXT_JS: &str = include_str!("../ui/plan-text.js");
const PLAN_BUILDER_JS: &str = include_str!("../ui/plan-builder.js");
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
    /// The sandboxed preview of what the task created (a UX prototype, or a review
    /// pack of decks and design documents), and which of the two it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_label: Option<&'static str>,
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
    previews: crate::preview::Previews,
    scheduler: std::sync::OnceLock<crate::schedule::Scheduler>,
    staging: tools::Staging,
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
            "shares": if self.info.serving { self.shares_json() } else { json!([]) },
            "ticket": ticket,
        })
    }

    /// This agent's shared folders, each with what it can preview ("prototype" or
    /// "review pack"), if anything.
    fn shares_json(&self) -> Value {
        let shares: Vec<Value> = self
            .agent
            .shares()
            .shares
            .iter()
            .map(|share| {
                let mut v = json!(share);
                let is = |rel: &str, dir: bool| {
                    std::fs::symlink_metadata(share.path.join(rel))
                        .is_ok_and(|m| if dir { m.is_dir() } else { m.is_file() })
                };
                v["preview"] = if is("review", true) || is("design", true) {
                    json!("review pack")
                } else if is("prototype/index.html", false) {
                    json!("prototype")
                } else {
                    Value::Null
                };
                // The owner's UI shows MCP servers without their secret values.
                v["mcp"] = json!(
                    share
                        .mcp
                        .iter()
                        .map(|(name, s)| json!({
                            "name": name,
                            "access": s.access,
                            "tools": s.tools,
                            "describe": s.describe(),
                        }))
                        .collect::<Vec<_>>()
                );
                v
            })
            .collect();
        json!(shares)
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
        previews: crate::preview::Previews::default(),
        scheduler: std::sync::OnceLock::new(),
        staging: tools::Staging::default(),
    });
    start_scheduler(&shared);

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
        .route("/api/schedules", get(list_schedules).post(add_schedule))
        .route(
            "/api/schedules/{name}",
            delete(remove_schedule)
                .patch(toggle_schedule)
                .put(edit_schedule),
        )
        .route("/api/schedules/{name}/run", post(run_schedule))
        .route("/api/schedule-preview", post(preview_when))
        .route("/api/runs", get(list_runs))
        .route("/api/runs/{id}", get(get_run))
        .route("/api/previews", post(create_preview))
        .route("/api/approvals/{id}", post(answer_approval))
        .route("/api/ops", get(ops_state))
        .route("/api/playbooks", get(list_playbooks))
        .route("/api/playbooks/{name}", post(render_playbook))
        .route("/api/kb/{topic}", get(kb_topic))
        .route("/api/plans/check", post(check_plan))
        .route("/api/github", get(github_state))
        .route("/api/github/login", post(github_sign_in))
        .route("/api/repos", post(link_repo))
        .route("/api/repos/{name}", delete(unlink_repo))
        .route("/api/repos/{name}/sync", post(sync_repo))
        .route("/api/repos/{name}/pr", post(open_pr))
        .route("/api/skills", get(list_skills).post(tools::add_skill))
        .route(
            "/api/skills/{name}",
            get(skill_text).delete(tools::remove_skill),
        )
        .route(
            "/api/plugins",
            get(tools::list_plugins).post(tools::install_plugin),
        )
        .route("/api/plugin-preview", post(tools::preview_plugin))
        .route(
            "/api/plugins/{name}",
            delete(tools::remove_plugin).patch(tools::toggle_plugin),
        )
        .route("/api/plugins/{name}/update", post(tools::update_plugin))
        .route("/api/mcp", get(tools::mcp_state).post(tools::offer_mcp))
        .route("/api/mcp/parse", post(tools::parse_mcp))
        .route(
            "/api/mcp/{share}/{name}",
            delete(tools::remove_mcp).patch(tools::edit_mcp),
        )
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
        .route(
            "/tools.js",
            get(|| async { asset("text/javascript; charset=utf-8", TOOLS_JS) }),
        )
        .route(
            "/plan-text.js",
            get(|| async { asset("text/javascript; charset=utf-8", PLAN_TEXT_JS) }),
        )
        .route(
            "/plan-builder.js",
            get(|| async { asset("text/javascript; charset=utf-8", PLAN_BUILDER_JS) }),
        )
        // Outside the token check: the unguessable capability in the path is the
        // authorization, and the content runs in a CSP sandbox (see crate::preview).
        .route("/preview/{cap}", get(preview_root))
        .route("/preview/{cap}/", get(preview_index))
        .route("/preview/{cap}/{*path}", get(preview_file))
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

fn is_capability(cap: &str) -> bool {
    cap.len() == 32 && cap.bytes().all(|b| b.is_ascii_hexdigit())
}

fn no_preview() -> Response {
    (
        StatusCode::NOT_FOUND,
        "There is no such preview (previews expire after 2 hours).",
    )
        .into_response()
}

async fn preview_root(Path(cap): Path<String>) -> Response {
    if !is_capability(&cap) {
        return no_preview();
    }
    axum::response::Redirect::permanent(&format!("/preview/{cap}/")).into_response()
}

async fn preview_index(State(s): State<AppState>, Path(cap): Path<String>) -> Response {
    serve_preview(&s, &cap, "")
}

async fn preview_file(
    State(s): State<AppState>,
    Path((cap, path)): Path<(String, String)>,
) -> Response {
    serve_preview(&s, &cap, &path)
}

/// A prototype file, in a CSP sandbox: an opaque origin without network access, so
/// it can't reach this UI's API or token.
fn serve_preview(s: &Shared, cap: &str, path: &str) -> Response {
    match s.previews.file(cap, path) {
        Some((kind, body)) => (
            [
                (header::CONTENT_TYPE, kind),
                (header::CACHE_CONTROL, "no-store"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::CONTENT_SECURITY_POLICY, crate::preview::PREVIEW_CSP),
                (header::REFERRER_POLICY, "no-referrer"),
            ],
            body,
        )
            .into_response(),
        None => no_preview(),
    }
}

#[derive(Deserialize)]
struct PreviewBody {
    /// A folder this agent shares: its prototype, decks and design documents now.
    share: Option<String>,
    /// A saved run and step: what the step's patch created.
    run: Option<String>,
    step: Option<String>,
    /// An open approval request that came with something to preview.
    approval: Option<u64>,
}

/// Opens a sandboxed preview of a UX prototype or a review pack; returns its
/// capability URL.
async fn create_preview(
    State(s): State<AppState>,
    Json(body): Json<PreviewBody>,
) -> Result<Json<Value>, ApiError> {
    let files = if let Some(id) = body.approval {
        s.agent
            .approval_preview(id)
            .ok_or_else(|| bad_request("that approval request has nothing to preview"))?
    } else if let (Some(run), Some(step)) = (&body.run, &body.step) {
        validate_name(run)
            .and(validate_name(step))
            .map_err(|_| bad_request("invalid run or step"))?;
        let path = s
            .info
            .profile
            .runs_dir()
            .join(run)
            .join(format!("{step}.patch"));
        let patch = std::fs::read_to_string(path)
            .map_err(|_| bad_request(format!("run {run} has no patch for step {step}")))?;
        crate::preview::site(&crate::preview::sources_from_patch(&patch))
    } else if let Some(name) = &body.share {
        let path = s
            .agent
            .shares()
            .get(name)
            .map(|share| share.path.clone())
            .ok_or_else(|| bad_request(format!("this agent shares no folder '{name}'")))?;
        tokio::task::spawn_blocking(move || {
            crate::preview::site(&crate::preview::sources_from_dir(&path))
        })
        .await
        .map_err(internal)?
    } else {
        return Err(bad_request("give a share, a run and step, or an approval"));
    };
    let cap = s.previews.add(files).ok_or_else(|| {
        bad_request(
            "there is nothing to preview (no prototype/index.html, review/ decks or design/ documents)",
        )
    })?;
    Ok(Json(json!({ "url": format!("/preview/{cap}/") })))
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
    v["builtin_checks"] = crate::spec::BUILT_IN_CHECKS
        .iter()
        .map(|name| json!({ "name": name, "about": crate::spec::about(name) }))
        .collect();
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
    /// For peers: MCP servers of that folder the model may use.
    #[serde(default)]
    mcp: Vec<String>,
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
    if body.target != "local" {
        task.mcp = body.mcp;
    }
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
        preview: None,
        preview_label: None,
    };
    s.tasks.lock().unwrap().push(view.clone());
    s.send(json!({ "type": "task", "task": view }));

    let shared = s.clone();
    let handle = tokio::spawn(async move {
        let mut evidence = None;
        let result = match route {
            Route::Local(executor) => {
                let ctx = TaskContext::new(shared.info.access, shared.info.workspace.clone());
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
    let started = start_plan(&s, body.text).map_err(bad_request)?;
    Ok(Json(
        json!({ "plan": started.plan, "steps": started.steps }),
    ))
}

/// A plan started by [`start_plan`].
struct StartedPlan {
    plan: u64,
    steps: HashMap<String, u64>,
    /// The saved run's verdict line and id, when the plan ends.
    done: tokio::task::JoinHandle<crate::schedule::Outcome>,
}

fn start_plan(s: &AppState, text: String) -> Result<StartedPlan, String> {
    let plan = Plan::parse(&text).map_err(|e| format!("{e:#}"))?;
    let kb = s.info.profile.knowledge();
    let skills = s.info.profile.skills();
    let squads_now = s.squads.lock().unwrap().clone();
    let errors: Vec<String> =
        crate::plan::preflight(&plan, &s.agent.peers(), &squads_now, &kb, &skills)
            .into_iter()
            .filter(|p| p.error)
            .map(|p| match p.step {
                Some(step) => format!("step {step}: {}", p.message),
                None => p.message,
            })
            .collect();
    if !errors.is_empty() {
        return Err(format!("the plan was not started:\n{}", errors.join("\n")));
    }
    let ready = plan.prepare(&kb, &skills).map_err(|e| format!("{e:#}"))?;
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
            preview: None,
            preview_label: None,
        };
        s.tasks.lock().unwrap().push(view.clone());
        s.send(json!({ "type": "task", "task": view }));
    }
    let squads = s.squads.lock().unwrap().clone();
    let shared = s.clone();
    let step_ids = ids.clone();
    let handle = tokio::spawn(async move {
        let started = crate::runs::unix_now();
        let states = run_plan(&shared.agent.clone(), &squads, &ready, |step, state| {
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
                    patch,
                } => {
                    t.status = "done";
                    t.target = peer.clone();
                    t.output = Some(output.clone());
                    t.finished_ms = Some(now_ms());
                    t.evidence = evidence
                        .as_ref()
                        .map(|_| if *verified { "verified" } else { "unverified" });
                    if let Some(patch) = patch.as_deref() {
                        let sources = crate::preview::sources_from_patch(patch);
                        t.preview_label = crate::preview::label(&sources);
                        t.preview = t
                            .preview_label
                            .and_then(|_| shared.previews.add(crate::preview::site(&sources)))
                            .map(|cap| format!("/preview/{cap}/"));
                    }
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
        let saved_run = match run.save(&shared.info.profile.runs_dir()) {
            Ok(_) => {
                shared.system(
                    if run.passed() { "info" } else { "warn" },
                    format!("Evidence run {}: {}", run.id, run.verdict_line()),
                );
                Some(run.id.clone())
            }
            Err(e) => {
                shared.system("warn", format!("Could not save the run: {e:#}"));
                None
            }
        };
        let saved = crate::plan::save_knowledge(&kb, &plan, &states, &run.id);
        if !saved.is_empty() {
            shared.system("info", format!("Knowledge saved: {}", saved.join(", ")));
        }
        shared.send(json!({ "type": "runs" }));
        shared.send(json!({ "type": "ops" }));
        crate::schedule::Outcome {
            verdict: run.verdict_line(),
            run_id: saved_run,
        }
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
    Ok(StartedPlan {
        plan: plan_id,
        steps: ids,
        done: handle,
    })
}

/// Runs the profile's scheduled plans like plans started here, so their steps show
/// up as tasks. Checks every 30 seconds (`P2PAGENTS_SCHEDULE_TICK_SECS` overrides it).
fn start_scheduler(shared: &AppState) {
    use crate::schedule::{Launch, Outcome, Schedule, Scheduler};
    let weak = Arc::downgrade(shared);
    let launch: Launch = Arc::new(move |schedule: Schedule| {
        let s = weak.upgrade().context("the agent stopped")?;
        let started = start_plan(&s, schedule.plan.clone()).map_err(|e| anyhow!(e))?;
        s.system(
            "task",
            format!(
                "Scheduled plan '{}' started ({})",
                schedule.name, schedule.when
            ),
        );
        Ok(Box::pin(async move {
            started.done.await.unwrap_or_else(|_| Outcome {
                verdict: "cancelled: the run was stopped".to_owned(),
                run_id: None,
            })
        }))
    });
    let weak = Arc::downgrade(shared);
    let scheduler = Scheduler::new(shared.info.profile.schedules_path(), launch).on_change(
        Arc::new(move || {
            if let Some(s) = weak.upgrade() {
                s.send(json!({ "type": "schedules" }));
            }
        }),
    );
    let tick = std::env::var("P2PAGENTS_SCHEDULE_TICK_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    scheduler.spawn(Duration::from_secs(tick));
    let _ = shared.scheduler.set(scheduler);
}

fn schedules_json(s: &Shared) -> Result<Value, ApiError> {
    let store = crate::schedule::ScheduleStore::load(&s.info.profile.schedules_path())
        .map_err(|e| bad_request(format!("{e:#}")))?;
    let running = |name: &str| s.scheduler.get().is_some_and(|x| x.is_running(name));
    Ok(json!(
        store
            .schedules
            .iter()
            .map(|sch| {
                let running = running(&sch.name);
                // A run recorded as running that isn't (the agent stopped mid-run).
                let status = match sch.last_status() {
                    _ if running => Some("running"),
                    Some("running") => Some("cancelled"),
                    status => status,
                };
                json!({
                    "name": sch.name,
                    "when": sch.when,
                    "when_parts": crate::schedule::when_parts(&sch.when),
                    "source": sch.source,
                    "enabled": sch.enabled,
                    "running": running,
                    "steps": crate::plan::Plan::parse(&sch.plan).map(|p| p.steps.len()).unwrap_or(0),
                    "plan": sch.plan,
                    "next_unix": sch.next_run().map(|t| t.timestamp()),
                    "next_runs": sch.next_runs(3).iter().map(|t| t.timestamp()).collect::<Vec<_>>(),
                    "retry_unix": sch.retry_unix,
                    "last_run_unix": sch.last_run_unix,
                    "last_result": sch.last_result,
                    "last_status": status,
                    "last_run_id": sch.last_run_id,
                })
            })
            .collect::<Vec<_>>()
    ))
}

async fn list_schedules(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "schedules": schedules_json(&s)? })))
}

/// Changes the profile's schedules under their lock, then tells the page.
fn change_schedules<T>(
    s: &Shared,
    change: impl FnOnce(&mut crate::schedule::ScheduleStore) -> anyhow::Result<T>,
) -> Result<T, ApiError> {
    let out = crate::schedule::ScheduleStore::update(&s.info.profile.schedules_path(), change)
        .map_err(|e| bad_request(format!("{e:#}")))?;
    s.send(json!({ "type": "schedules" }));
    Ok(out)
}

#[derive(Deserialize)]
struct ScheduleBody {
    name: String,
    when: String,
    plan: String,
    #[serde(default)]
    source: String,
}

async fn add_schedule(
    State(s): State<AppState>,
    Json(body): Json<ScheduleBody>,
) -> Result<Json<Value>, ApiError> {
    let source = if body.source.trim().is_empty() {
        "web UI".to_owned()
    } else {
        body.source
    };
    let schedule = crate::schedule::Schedule::new(&body.name, &body.when, &body.plan, &source)
        .map_err(|e| bad_request(format!("{e:#}")))?;
    let next = schedule.next_run();
    change_schedules(&s, |store| {
        if store.get(&schedule.name).is_some() {
            bail!(
                "a schedule named '{}' already exists: edit it, or pick another name",
                schedule.name
            );
        }
        store.add(schedule);
        Ok(())
    })?;
    s.system(
        "info",
        format!(
            "Scheduled plan '{}' ({}); next run {}",
            body.name,
            body.when.trim(),
            next.map(|t| t.format("%a %Y-%m-%d %H:%M").to_string())
                .unwrap_or_default()
        ),
    );
    Ok(Json(json!({ "schedules": schedules_json(&s)? })))
}

#[derive(Deserialize)]
struct ScheduleEdit {
    #[serde(default)]
    when: Option<String>,
    #[serde(default)]
    plan: Option<String>,
}

/// Changes when a schedule runs (counting from now) and/or the plan it runs.
async fn edit_schedule(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<ScheduleEdit>,
) -> Result<Json<Value>, ApiError> {
    change_schedules(&s, |store| {
        let schedule = store
            .get_mut(&name)
            .with_context(|| format!("no schedule named '{name}'"))?;
        if let Some(plan) = &body.plan {
            schedule.set_plan(plan)?;
        }
        if let Some(when) = body.when.as_deref().filter(|w| w.trim() != schedule.when) {
            schedule.set_when(when)?;
        }
        Ok(())
    })?;
    Ok(Json(json!({ "schedules": schedules_json(&s)? })))
}

#[derive(Deserialize)]
struct WhenBody {
    when: String,
}

/// What `when` means: its parts and its next three run times, or why it's invalid.
async fn preview_when(Json(body): Json<WhenBody>) -> Json<Value> {
    Json(
        match crate::schedule::upcoming(&body.when, &chrono::Local::now(), 3) {
            Ok(next) => json!({
                "ok": true,
                "parts": crate::schedule::when_parts(&body.when),
                "next": next.iter().map(|t| t.timestamp()).collect::<Vec<_>>(),
            }),
            Err(e) => json!({ "ok": false, "error": format!("{e:#}") }),
        },
    )
}

#[derive(Deserialize)]
struct ToggleBody {
    enabled: bool,
}

/// Pauses a schedule, or resumes it counting from now (paused runs are not caught up).
async fn toggle_schedule(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<ToggleBody>,
) -> Result<Json<Value>, ApiError> {
    change_schedules(&s, |store| {
        store
            .get_mut(&name)
            .with_context(|| format!("no schedule named '{name}'"))?
            .set_enabled(body.enabled);
        Ok(())
    })?;
    Ok(Json(json!({ "schedules": schedules_json(&s)? })))
}

async fn run_schedule(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let scheduler = s
        .scheduler
        .get()
        .ok_or_else(|| bad_request("schedules are not running"))?;
    scheduler
        .trigger(&name)
        .map_err(|e| bad_request(format!("{e:#}")))?;
    Ok(Json(json!({ "schedules": schedules_json(&s)? })))
}

async fn remove_schedule(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    change_schedules(&s, |store| {
        if !store.remove(&name) {
            bail!("no schedule named '{name}'");
        }
        Ok(())
    })?;
    Ok(Json(json!({ "schedules": schedules_json(&s)? })))
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

fn internal(e: impl std::fmt::Display) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
}

/// The health dashboard: monitors against their SLOs (last 24 hours), incident
/// events, peer task statistics, evidence runs and the knowledge base topics.
async fn ops_state(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    use crate::ops::{MonitorStore, monitor_stats, task_stats};
    let profile = &s.info.profile;
    let since = crate::runs::unix_now().saturating_sub(24 * 3600);
    let monitors = MonitorStore::load(&profile.monitors_path())
        .map_err(internal)?
        .monitors;
    let store = profile.ops_store();
    let incidents = store.incidents(since);
    // The whole log: an incident opened days ago may still be open.
    let stats = monitor_stats(
        &monitors,
        &store.samples(since),
        &store.incidents(0),
        &s.agent.shares(),
    );
    let audit: Vec<_> = AuditLog::read(&profile.audit_path(), 10_000)
        .unwrap_or_default()
        .into_iter()
        .filter(|e| e.ts_ms >= since * 1000)
        .collect();
    let runs = RunRecord::list(&profile.runs_dir()).unwrap_or_default();
    let recent: Vec<_> = incidents.iter().rev().take(30).cloned().collect();
    Ok(Json(json!({
        // Monitors with an open incident (one ongoing problem each).
        "open_incidents": stats.iter().filter(|m| m.open_incident).count(),
        "monitors": stats,
        "incidents": recent,
        "tasks": task_stats(&audit),
        "runs": { "total": runs.len(), "passed": runs.iter().filter(|r| r.passed).count() },
        "knowledge": profile.knowledge().list(),
    })))
}

/// Parses a plan and checks it against the connected peers, their folders and
/// checks, squads, knowledge and skills, without running it.
async fn check_plan(State(s): State<AppState>, Json(body): Json<PlanBody>) -> Json<Value> {
    let plan = match Plan::parse(&body.text) {
        Ok(plan) => plan,
        Err(e) => {
            return Json(json!({
                "ok": false,
                "problems": [{ "step": null, "error": true, "message": format!("{e:#}") }],
            }));
        }
    };
    let squads = s.squads.lock().unwrap().clone();
    let problems = crate::plan::preflight(
        &plan,
        &s.agent.peers(),
        &squads,
        &s.info.profile.knowledge(),
        &s.info.profile.skills(),
    );
    Json(json!({
        "ok": !problems.iter().any(|p| p.error),
        "problems": problems,
        "steps": plan.steps.len(),
        "claims": plan.claims.len(),
    }))
}

/// GitHub: the profile's account, the GitHub CLI accounts it could use, and the
/// linked repositories with the branches agents' tasks made.
async fn github_state(State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    use crate::github::{RepoStore, gh_accounts, task_branches};
    let profile = s.info.profile.reload().map_err(internal)?;
    let account = match &profile.config.github {
        Some(crate::profile::GithubAuth::Gh { user, host }) => {
            json!({ "user": user, "host": host })
        }
        None if profile.config.owner.is_some() => json!({ "owner": profile.config.owner }),
        None => Value::Null,
    };
    let accounts: Vec<Value> = tokio::task::spawn_blocking(gh_accounts)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(login, host, active)| json!({ "login": login, "host": host, "active": active }))
        .collect();
    let mut repos = Vec::new();
    for repo in RepoStore::load(&profile.repos_path())
        .map_err(internal)?
        .repos
    {
        let branches = task_branches(&repo).await.unwrap_or_default();
        repos.push(json!({
            "name": repo.name, "slug": repo.slug, "host": repo.host, "branch": repo.branch,
            "path": repo.path.display().to_string(), "branches": branches,
        }));
    }
    Ok(Json(json!({
        "account": account,
        "accounts": accounts,
        "repos": repos,
        "serving": s.info.serving,
    })))
}

#[derive(Deserialize)]
struct GithubLoginBody {
    user: String,
    host: String,
}

async fn github_sign_in(
    State(s): State<AppState>,
    Json(body): Json<GithubLoginBody>,
) -> Result<Json<Value>, ApiError> {
    let mut profile = s.info.profile.reload().map_err(internal)?;
    let login = tokio::task::spawn_blocking(move || {
        profile
            .github_login(&body.user, &body.host)
            .map(|l| (l, body.host))
    })
    .await
    .map_err(internal)?
    .map_err(|e| bad_request(format!("{e:#}")))?;
    s.system(
        "info",
        format!("Signed in to GitHub as {}@{}", login.0, login.1),
    );
    Ok(Json(json!({ "login": login.0 })))
}

#[derive(Deserialize)]
struct LinkBody {
    repo: String,
    name: Option<String>,
    #[serde(default)]
    access: Access,
    /// `name=command` lines.
    #[serde(default)]
    checks: Vec<String>,
    /// Allowlisted peers to grant the folder to.
    #[serde(default)]
    grant: Vec<String>,
}

async fn link_repo(
    State(s): State<AppState>,
    Json(body): Json<LinkBody>,
) -> Result<Json<Value>, ApiError> {
    let profile = s.info.profile.reload().map_err(internal)?;
    let checks: Vec<String> = body
        .checks
        .into_iter()
        .filter(|c| !c.trim().is_empty())
        .collect();
    let checks = crate::shares::parse_checks(&checks).map_err(|e| bad_request(format!("{e:#}")))?;
    let name = body.name.filter(|n| !n.trim().is_empty());
    let (repo, shares) =
        crate::github::link(&profile, &body.repo, name.as_deref(), body.access, checks)
            .await
            .map_err(|e| bad_request(format!("{e:#}")))?;
    s.agent.set_shares(shares);
    for peer in &body.grant {
        let Some(mut entry) = s.agent.trusted().get_by_name(peer).cloned() else {
            continue;
        };
        if !entry.shares.contains(&repo.name) {
            entry.shares.push(repo.name.clone());
        }
        s.agent.allow(entry);
    }
    s.save_trust()?;
    s.system(
        "info",
        format!(
            "Linked {} as folder {} (a worktree per task)",
            repo.slug, repo.name
        ),
    );
    s.broadcast_peers();
    Ok(Json(json!({ "name": repo.name, "branch": repo.branch })))
}

async fn unlink_repo(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let profile = s.info.profile.reload().map_err(internal)?;
    let shares =
        crate::github::unlink(&profile, &name, false).map_err(|e| bad_request(format!("{e:#}")))?;
    s.agent.set_shares(shares);
    s.system("info", format!("Unlinked {name} (the clone stays on disk)"));
    Ok(Json(json!({ "unlinked": name })))
}

fn linked(
    s: &Shared,
    name: &str,
) -> Result<(crate::github::Repo, crate::github::GithubCredentials), ApiError> {
    let profile = s.info.profile.reload().map_err(internal)?;
    let repo = crate::github::RepoStore::load(&profile.repos_path())
        .map_err(internal)?
        .get(name)
        .map_err(|e| bad_request(format!("{e:#}")))?
        .clone();
    let creds = profile
        .github()
        .map_err(|e| bad_request(format!("{e:#}")))?;
    Ok((repo, creds))
}

async fn sync_repo(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (repo, creds) = linked(&s, &name)?;
    let head = creds
        .sync(&repo)
        .await
        .map_err(|e| bad_request(format!("{e:#}")))?;
    Ok(Json(json!({ "head": head, "branch": repo.branch })))
}

#[derive(Deserialize)]
struct PrBody {
    branch: String,
    title: String,
    #[serde(default)]
    body: String,
}

async fn open_pr(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(body): Json<PrBody>,
) -> Result<Json<Value>, ApiError> {
    let (repo, creds) = linked(&s, &name)?;
    if body.title.trim().is_empty() {
        return Err(bad_request("the pull request needs a title"));
    }
    creds
        .push(&repo, &body.branch)
        .await
        .map_err(|e| bad_request(format!("{e:#}")))?;
    let url = creds
        .create_pr(&repo, &body.branch, body.title.trim(), &body.body)
        .await
        .map_err(|e| bad_request(format!("{e:#}")))?;
    s.system("info", format!("Opened pull request {url}"));
    Ok(Json(json!({ "url": url })))
}

async fn list_skills(State(s): State<AppState>) -> Json<Value> {
    Json(json!({ "skills": s.info.profile.skills().list() }))
}

async fn skill_text(
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let text = s.info.profile.skills().text(&name).map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("{e:#}") })),
        )
    })?;
    Ok(Json(json!({ "name": name, "text": text })))
}

async fn list_playbooks() -> Result<Json<Value>, ApiError> {
    let mut list = Vec::new();
    for (name, description) in crate::playbooks::list() {
        let vars: Vec<Value> = crate::playbooks::variables(name)
            .map_err(internal)?
            .into_iter()
            .map(|(v, d)| json!({ "name": v, "default": d }))
            .collect();
        list.push(json!({
            "name": name,
            "description": description,
            "vars": vars,
            "outline": crate::playbooks::outline(name).map_err(internal)?,
        }));
    }
    Ok(Json(json!({ "playbooks": list })))
}

#[derive(Deserialize)]
struct PlaybookBody {
    #[serde(default)]
    vars: std::collections::BTreeMap<String, String>,
}

async fn render_playbook(
    Path(name): Path<String>,
    Json(body): Json<PlaybookBody>,
) -> Result<Json<Value>, ApiError> {
    let vars = body
        .vars
        .into_iter()
        .filter(|(_, v)| !v.trim().is_empty())
        .collect();
    let text = crate::playbooks::render(&name, &vars).map_err(|e| bad_request(format!("{e:#}")))?;
    Ok(Json(json!({ "text": text })))
}

async fn kb_topic(
    State(s): State<AppState>,
    Path(topic): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let text = s.info.profile.knowledge().read(&topic).map_err(|e| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("{e:#}") })),
        )
    })?;
    Ok(Json(json!({ "topic": topic, "text": text })))
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
    // Steps whose patch created something to preview (POST /api/previews opens it).
    let previews: Vec<Value> = run
        .steps
        .iter()
        .map(|s| s.step.id.as_str())
        .filter_map(|step| {
            let patch =
                std::fs::read_to_string(dir.join(&run.id).join(format!("{step}.patch"))).ok()?;
            let label = crate::preview::label(&crate::preview::sources_from_patch(&patch))?;
            Some(json!({ "step": step, "label": label }))
        })
        .collect();
    Ok(Json(json!({
        "summary": run.summary(),
        "verdict": run.verdict_line(),
        "report": run.render_markdown(),
        "signatures": checks,
        "previews": previews,
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
