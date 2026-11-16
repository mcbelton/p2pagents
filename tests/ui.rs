//! Web UI server: access token, Host check, security headers and the task API.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use p2pagents::audit::{AuditEntry, AuditLog};
use p2pagents::shares::ShareStore;
use p2pagents::ui::{self, UiInfo};
use p2pagents::{
    Access, Agent, AgentConfig, Executor, LlmTask, Network, Profile, Provider, SecretKey,
    TaskContext, TrustedPeer,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Echo;

#[async_trait]
impl Executor for Echo {
    fn providers(&self) -> Vec<Provider> {
        vec![Provider::Copilot]
    }

    async fn execute(&self, task: &LlmTask, _ctx: &TaskContext) -> anyhow::Result<String> {
        if task.prompt == "hang" {
            std::future::pending::<()>().await;
        }
        Ok(format!("echo: {}", task.prompt))
    }
}

struct Response {
    status: u16,
    headers: String,
    body: String,
}

impl Response {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("JSON body")
    }
}

async fn request(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> Response {
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(token) = token {
        req += &format!("Authorization: Bearer {token}\r\n");
    }
    let body = body.unwrap_or("");
    if !body.is_empty() {
        req += &format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        );
    }
    req += "\r\n";
    req += body;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).await.unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").expect("HTTP response");
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    Response {
        status,
        headers: head.to_ascii_lowercase(),
        body: body.to_owned(),
    }
}

#[tokio::test]
async fn ui_requires_token_and_local_host_and_runs_tasks() {
    let home = tempfile::tempdir().unwrap();
    let mut config = AgentConfig::new("solo", SecretKey::generate());
    config.network = Network::Local;
    config.bind = Some("127.0.0.1:0".parse().unwrap());
    let executor: Arc<dyn Executor> = Arc::new(Echo);
    config.executor = Some(executor.clone());
    let (agent, events) = Agent::bind(config).await.unwrap();
    let info = UiInfo {
        profile: Profile::open_in(home.path(), "solo").unwrap(),
        network: Network::Local,
        serving: false,
        access: Access::ReadOnly,
        workspace: home.path().to_path_buf(),
        providers: vec![Provider::Copilot],
        accounts: Vec::new(),
    };
    let server = ui::start(
        agent.clone(),
        events,
        info,
        Some(executor),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();

    let rest = server.url.strip_prefix("http://127.0.0.1:").unwrap();
    let (port, token) = rest.split_once("/#t=").unwrap();
    let port: u16 = port.parse().unwrap();
    assert_eq!(token.len(), 32);
    let host = format!("127.0.0.1:{port}");

    let page = request(port, "GET", "/", &host, None, None).await;
    assert_eq!(page.status, 200);
    assert!(
        page.headers
            .contains("content-security-policy: default-src 'self'")
    );
    assert!(page.headers.contains("x-content-type-options: nosniff"));

    let anonymous = request(port, "GET", "/api/state", &host, None, None).await;
    assert_eq!(anonymous.status, 401);
    let wrong = request(port, "GET", "/api/state", &host, Some("0123"), None).await;
    assert_eq!(wrong.status, 401);
    let rebound = request(
        port,
        "GET",
        "/api/state",
        "evil.example:80",
        Some(token),
        None,
    )
    .await;
    assert_eq!(rebound.status, 403, "foreign Host must be refused");

    let state = request(port, "GET", "/api/state", &host, Some(token), None).await;
    assert_eq!(state.status, 200);
    assert_eq!(state.json()["me"]["name"], "solo");
    let localhost = format!("localhost:{port}");
    let state = request(port, "GET", "/api/state", &localhost, Some(token), None).await;
    assert_eq!(state.status, 200);

    let empty = r#"{"provider":"copilot","prompt":"  ","target":"local"}"#;
    let rejected = request(port, "POST", "/api/tasks", &host, Some(token), Some(empty)).await;
    assert_eq!(rejected.status, 400);

    let task = r#"{"provider":"copilot","prompt":"hello","target":"local"}"#;
    let created = request(port, "POST", "/api/tasks", &host, Some(token), Some(task)).await;
    assert_eq!(created.status, 200, "{}", created.body);

    let done = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state = request(port, "GET", "/api/state", &host, Some(token), None)
                .await
                .json();
            let task = state["tasks"][0].clone();
            if task["status"] != "running" {
                return task;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("task finished");
    assert_eq!(done["status"], "done");
    assert_eq!(done["output"], "echo: hello");
    agent.shutdown().await;
}

#[tokio::test]
async fn delegate_only_agents_allowlist_chat_peers_from_the_ui() {
    let home = tempfile::tempdir().unwrap();
    let mut config = AgentConfig::new("boss", SecretKey::generate());
    config.network = Network::Local;
    config.bind = Some("127.0.0.1:0".parse().unwrap());
    let (agent, events) = Agent::bind(config).await.unwrap();
    let info = UiInfo {
        profile: Profile::open_in(home.path(), "boss").unwrap(),
        network: Network::Local,
        serving: false,
        access: Access::ReadOnly,
        workspace: home.path().to_path_buf(),
        providers: Vec::new(),
        accounts: Vec::new(),
    };
    let server = ui::start(
        agent.clone(),
        events,
        info,
        None,
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    let rest = server.url.strip_prefix("http://127.0.0.1:").unwrap();
    let (port, token) = rest.split_once("/#t=").unwrap();
    let port: u16 = port.parse().unwrap();
    let host = format!("127.0.0.1:{port}");
    let peer = SecretKey::generate().public();

    let chat = format!(r#"{{"ticket":"{peer}","name":"pal"}}"#);
    let added = request(port, "POST", "/api/trust", &host, Some(token), Some(&chat)).await;
    assert_eq!(added.status, 200, "{}", added.body);
    assert!(agent.trusted().get(&peer).is_some_and(|p| !p.can_delegate));

    let delegate =
        format!(r#"{{"ticket":"{peer}","name":"pal","delegate":true,"shares":["nope"]}}"#);
    let refused = request(
        port,
        "POST",
        "/api/trust",
        &host,
        Some(token),
        Some(&delegate),
    )
    .await;
    assert_eq!(refused.status, 400);
    assert!(
        refused.body.contains("no folder named 'nope'"),
        "{}",
        refused.body
    );

    agent.shutdown().await;
}

#[tokio::test]
async fn ui_cancels_running_tasks_and_shows_the_audit_log() {
    let home = tempfile::tempdir().unwrap();
    let profile = Profile::open_in(home.path(), "solo").unwrap();
    // Two entries a peer left in this profile's audit log.
    let log = AuditLog::new(profile.audit_path());
    let peer = SecretKey::generate().public();
    let task = LlmTask::new(Provider::Copilot, "private prompt text");
    let started = std::time::SystemTime::now();
    log.record(&AuditEntry::new(
        "bob",
        &peer,
        1,
        &task,
        Some(Access::ReadOnly),
        &Ok("x".into()),
        started,
    ));
    log.record(&AuditEntry::new(
        "bob",
        &peer,
        2,
        &task,
        None,
        &Err(anyhow::anyhow!("not allowed")),
        started,
    ));

    let mut config = AgentConfig::new("solo", SecretKey::generate());
    config.network = Network::Local;
    config.bind = Some("127.0.0.1:0".parse().unwrap());
    let executor: Arc<dyn Executor> = Arc::new(Echo);
    let (agent, events) = Agent::bind(config).await.unwrap();
    let info = UiInfo {
        profile,
        network: Network::Local,
        serving: true,
        access: Access::ReadOnly,
        workspace: home.path().to_path_buf(),
        providers: vec![Provider::Copilot],
        accounts: Vec::new(),
    };
    let server = ui::start(
        agent.clone(),
        events,
        info,
        Some(executor),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    let rest = server.url.strip_prefix("http://127.0.0.1:").unwrap();
    let (port, token) = rest.split_once("/#t=").unwrap();
    let port: u16 = port.parse().unwrap();
    let host = format!("127.0.0.1:{port}");

    // Cancel a running task.
    let hang = r#"{"provider":"copilot","prompt":"hang","target":"local"}"#;
    let created = request(port, "POST", "/api/tasks", &host, Some(token), Some(hang)).await;
    assert_eq!(created.status, 200, "{}", created.body);
    let id = created.json()["id"].as_u64().unwrap();
    let path = format!("/api/tasks/{id}");
    assert_eq!(
        request(port, "DELETE", &path, &host, None, None)
            .await
            .status,
        401
    );
    let cancelled = request(port, "DELETE", &path, &host, Some(token), None).await;
    assert_eq!(cancelled.status, 200, "{}", cancelled.body);
    let state = request(port, "GET", "/api/state", &host, Some(token), None)
        .await
        .json();
    assert_eq!(state["tasks"][0]["status"], "cancelled");
    let again = request(port, "DELETE", &path, &host, Some(token), None).await;
    assert_eq!(again.status, 409, "a finished task can't be cancelled");

    // The audit log, newest first, only with the token, never with prompts.
    assert_eq!(
        request(port, "GET", "/api/audit", &host, None, None)
            .await
            .status,
        401
    );
    let audit = request(port, "GET", "/api/audit", &host, Some(token), None).await;
    assert_eq!(audit.status, 200);
    let body = audit.json();
    assert_eq!(body["enabled"], true);
    assert_eq!(body["entries"][0]["outcome"], "refused");
    assert_eq!(body["entries"][1]["outcome"], "done");
    assert!(!audit.body.contains("private prompt text"));

    agent.shutdown().await;
}

#[tokio::test]
async fn ui_runs_plans_and_shows_squads() {
    let home = tempfile::tempdir().unwrap();
    let (boss_key, worker_key) = (SecretKey::generate(), SecretKey::generate());
    let trust = |name: &str, key: &SecretKey, delegate: bool| TrustedPeer {
        name: name.into(),
        id: key.public(),
        addrs: Default::default(),
        can_delegate: delegate,
        tasks_per_hour: 100,
        access: Access::ReadOnly,
        shares: vec!["data".into()],
    };
    let mut worker = AgentConfig::new("worker", worker_key.clone());
    worker.network = Network::Local;
    worker.bind = Some("127.0.0.1:0".parse().unwrap());
    worker.trust.allow(trust("boss", &boss_key, true));
    worker.executor = Some(Arc::new(Echo));
    worker.shares = ShareStore::with_data(home.path().join("data"));
    let (worker, _we) = Agent::bind(worker).await.unwrap();

    let mut config = AgentConfig::new("boss", boss_key);
    config.network = Network::Local;
    config.bind = Some("127.0.0.1:0".parse().unwrap());
    config.trust.allow(trust("worker", &worker_key, false));
    let (boss, events) = Agent::bind(config).await.unwrap();
    boss.connect(worker.addr()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while boss.peers().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let info = UiInfo {
        profile: Profile::open_in(home.path(), "boss").unwrap(),
        network: Network::Local,
        serving: false,
        access: Access::ReadOnly,
        workspace: home.path().to_path_buf(),
        providers: Vec::new(),
        accounts: Vec::new(),
    };
    let server = ui::start(
        boss.clone(),
        events,
        info,
        None,
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    let rest = server.url.strip_prefix("http://127.0.0.1:").unwrap();
    let (port, token) = rest.split_once("/#t=").unwrap();
    let port: u16 = port.parse().unwrap();
    let host = format!("127.0.0.1:{port}");
    let post = |path: &'static str, body: String| {
        let host = host.clone();
        let token = token.to_owned();
        async move { request(port, "POST", path, &host, Some(&token), Some(&body)).await }
    };

    // Squads: only allowlisted members; shown with who is online.
    let bad = post(
        "/api/squads",
        r#"{"name":"qa","members":["stranger"]}"#.into(),
    )
    .await;
    assert_eq!(bad.status, 400);
    let made = post(
        "/api/squads",
        r#"{"name":"qa","members":["worker"]}"#.into(),
    )
    .await;
    assert_eq!(made.status, 200, "{}", made.body);
    let state = request(port, "GET", "/api/state", &host, Some(token), None)
        .await
        .json();
    assert_eq!(state["squads"][0]["name"], "qa");
    assert_eq!(state["squads"][0]["members"][0]["online"], true);
    assert!(
        home.path().join("boss/squads.json").is_file(),
        "saved to the profile"
    );

    // Plans: invalid ones are rejected before anything runs.
    let cycle = post(
        "/api/plans",
        r#"{"text":"a after b @worker x\nb after a @worker y"}"#.into(),
    )
    .await;
    assert_eq!(cycle.status, 400);
    assert!(cycle.body.contains("cycle"), "{}", cycle.body);
    let text = serde_json::json!({ "text": "find @worker find the facts\nsum after find #qa summarize them" });
    let started = post("/api/plans", text.to_string()).await;
    assert_eq!(started.status, 200, "{}", started.body);

    let tasks = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state = request(port, "GET", "/api/state", &host, Some(token), None)
                .await
                .json();
            let tasks = state["tasks"].as_array().unwrap().clone();
            if tasks.len() == 2 && tasks.iter().all(|t| t["status"] == "done") {
                return tasks;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("plan finished");
    let step = |id: &str| tasks.iter().find(|t| t["step"] == id).unwrap().clone();
    assert_eq!(step("find")["target"], "worker");
    assert_eq!(step("sum")["after"][0], "find");
    let sum = step("sum")["output"].as_str().unwrap().to_owned();
    assert!(
        sum.contains("### Step find (done by worker)\necho: find the facts"),
        "{sum}"
    );
    assert_eq!(step("find")["evidence"], "verified", "signed by the worker");

    // An evidence run with a sign-off by this agent's own user (@me).
    let runs = || {
        let host = host.clone();
        let token = token.to_owned();
        async move {
            request(port, "GET", "/api/runs", &host, Some(&token), None)
                .await
                .json()["runs"]
                .as_array()
                .unwrap()
                .clone()
        }
    };
    assert_eq!(runs().await.len(), 1, "the first plan was saved as a run");
    let text = serde_json::json!({
        "text": "claim: C-1 the facts are reviewed\nfind @worker find the facts\nok after find approve @me claims=C-1 Approve the facts"
    });
    assert_eq!(post("/api/plans", text.to_string()).await.status, 200);
    let approval = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state = request(port, "GET", "/api/state", &host, Some(token), None)
                .await
                .json();
            if let Some(a) = state["approvals"].as_array().and_then(|a| a.first()) {
                return a.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("approval requested");
    assert!(
        approval["text"]
            .as_str()
            .unwrap()
            .contains("echo: find the facts")
    );
    let path: &'static str =
        Box::leak(format!("/api/approvals/{}", approval["id"]).into_boxed_str());
    let no_reason = post(path, r#"{"approved":false,"note":" "}"#.into()).await;
    assert_eq!(no_reason.status, 400, "a rejection needs a reason");
    let approved = post(path, r#"{"approved":true,"note":"checked"}"#.into()).await;
    assert_eq!(approved.status, 200, "{}", approved.body);
    let run = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let runs = runs().await;
            if runs.len() == 2 {
                return runs[0].clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("run saved");
    assert_eq!(run["passed"], true, "{run}");
    let id = run["id"].as_str().unwrap();
    let detail = request(
        port,
        "GET",
        &format!("/api/runs/{id}"),
        &host,
        Some(token),
        None,
    )
    .await
    .json();
    assert!(
        detail["verdict"]
            .as_str()
            .unwrap()
            .starts_with("PASS: 1/1 claims proven")
    );
    assert!(
        detail["report"]
            .as_str()
            .unwrap()
            .contains("Approved by boss: checked")
    );
    let signatures = detail["signatures"].as_array().unwrap();
    assert_eq!(signatures.len(), 2);
    assert!(signatures.iter().all(|s| s["ok"] == true), "{signatures:?}");
    assert!(
        home.path()
            .join("boss/runs")
            .join(id)
            .join("report.md")
            .is_file()
    );
    let missing = request(port, "GET", "/api/runs/nope", &host, Some(token), None).await;
    assert_eq!(missing.status, 404);

    let removed = request(port, "DELETE", "/api/squads/qa", &host, Some(token), None).await;
    assert_eq!(removed.status, 200);
    boss.shutdown().await;
    worker.shutdown().await;
}

#[tokio::test]
async fn ui_shows_health_knowledge_and_fills_in_playbooks() {
    use p2pagents::ops::{IncidentEvent, IncidentKind, Monitor, MonitorStore, Sample};
    let home = tempfile::tempdir().unwrap();
    let profile = Profile::open_in(home.path(), "solo").unwrap();
    let app = tempfile::tempdir().unwrap();
    let mut shares = ShareStore::with_data(home.path().join("data"));
    let checks = [("health".to_owned(), "exit 0".to_owned())].into();
    shares
        .add_with("app", app.path(), Access::Full, checks, false)
        .unwrap();
    let mut monitors = MonitorStore::default();
    let monitor = Monitor {
        name: "api".into(),
        share: "app".into(),
        check: "health".into(),
        every_secs: 60,
        fail_after: 3,
        slo: 99.0,
    };
    monitors.set(monitor, &shares).unwrap();
    monitors.save(&profile.monitors_path()).unwrap();
    let now = p2pagents::runs::unix_now();
    let store = profile.ops_store();
    for ok in [true, false] {
        store
            .record_sample(&Sample {
                ts: now,
                monitor: "api".into(),
                ok,
                ms: 120,
                exit: Some(if ok { 0 } else { 1 }),
            })
            .unwrap();
    }
    store
        .record_incident(&IncidentEvent {
            ts: now,
            incident: now,
            monitor: "api".into(),
            kind: IncidentKind::Opened,
            text: "api failed".into(),
            checks: vec![],
        })
        .unwrap();
    profile
        .knowledge()
        .write("architecture", "Two services.", "test")
        .unwrap();

    let mut config = AgentConfig::new("solo", SecretKey::generate());
    config.network = Network::Local;
    config.bind = Some("127.0.0.1:0".parse().unwrap());
    let (agent, events) = Agent::bind(config).await.unwrap();
    let info = UiInfo {
        profile,
        network: Network::Local,
        serving: false,
        access: Access::ReadOnly,
        workspace: home.path().to_path_buf(),
        providers: Vec::new(),
        accounts: Vec::new(),
    };
    let server = ui::start(
        agent.clone(),
        events,
        info,
        None,
        "127.0.0.1:0".parse().unwrap(),
    )
    .await
    .unwrap();
    let rest = server.url.strip_prefix("http://127.0.0.1:").unwrap();
    let (port, token) = rest.split_once("/#t=").unwrap();
    let port: u16 = port.parse().unwrap();
    let host = format!("127.0.0.1:{port}");
    let get = |path: &str| {
        let (host, token, path) = (host.clone(), token.to_owned(), path.to_owned());
        async move { request(port, "GET", &path, &host, Some(&token), None).await }
    };

    assert_eq!(
        request(port, "GET", "/api/ops", &host, None, None)
            .await
            .status,
        401
    );
    let ops = get("/api/ops").await.json();
    assert_eq!(ops["monitors"][0]["name"], "api");
    assert_eq!(ops["monitors"][0]["availability"], 50.0);
    assert_eq!(ops["monitors"][0]["meets_slo"], false);
    assert_eq!(ops["open_incidents"], 1);
    assert_eq!(ops["incidents"][0]["kind"], "opened");
    assert_eq!(ops["knowledge"][0]["name"], "architecture");
    let topic = get("/api/kb/architecture").await.json();
    assert!(topic["text"].as_str().unwrap().contains("Two services."));
    assert_eq!(get("/api/kb/missing").await.status, 404);

    let books = get("/api/playbooks").await.json();
    let names: Vec<&str> = books["playbooks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["bootstrap", "discover", "sdlc", "learn"]);
    let discover = &books["playbooks"][1]["outline"];
    assert!(
        discover["steps"][0]
            .as_str()
            .unwrap()
            .contains("the peer you pick")
    );
    assert_eq!(discover["claims"].as_array().unwrap().len(), 4);
    let post = |path: &str, body: &str| {
        let (host, token, path, body) = (
            host.clone(),
            token.to_owned(),
            path.to_owned(),
            body.to_owned(),
        );
        async move { request(port, "POST", &path, &host, Some(&token), Some(&body)).await }
    };
    let filled = post(
        "/api/playbooks/discover",
        r#"{"vars":{"peer":"alice","folder":"repo"}}"#,
    )
    .await;
    assert_eq!(filled.status, 200, "{}", filled.body);
    assert!(
        filled.json()["text"]
            .as_str()
            .unwrap()
            .contains("@alice:repo")
    );
    let missing = post("/api/playbooks/sdlc", r#"{"vars":{"peer":"alice"}}"#).await;
    assert_eq!(missing.status, 400);
    assert!(missing.body.contains("needs: change"), "{}", missing.body);

    // Plans can only use knowledge that exists.
    let unknown = post("/api/plans", r#"{"text":"a @alice context=nope do it"}"#).await;
    assert_eq!(unknown.status, 400);
    assert!(unknown.body.contains("no topic 'nope'"), "{}", unknown.body);

    // The plan check lists every problem without running anything.
    let checked = post(
        "/api/plans/check",
        r#"{"text":"claim: C-1 x\na @alice skill=nope do it"}"#,
    )
    .await
    .json();
    assert_eq!(checked["ok"], false);
    let messages: Vec<String> = checked["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["message"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("alice is not connected")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("no skill 'nope'")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.contains("no step proves claim C-1")),
        "{messages:?}"
    );
    let ok = post(
        "/api/plans/check",
        r#"{"text":"ok approve @me Approve it"}"#,
    )
    .await
    .json();
    assert_eq!(ok["ok"], true, "{ok}");
    let broken = post("/api/plans/check", r#"{"text":"nonsense"}"#)
        .await
        .json();
    assert_eq!(broken["ok"], false);

    // Skills: built-in ones, viewable.
    let skills = get("/api/skills").await.json();
    assert!(
        skills["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "security-review")
    );
    let skill = get("/api/skills/security-review").await.json();
    assert!(
        skill["text"]
            .as_str()
            .unwrap()
            .contains("# Security review")
    );
    assert_eq!(get("/api/skills/nope").await.status, 404);

    // GitHub: not signed in yet, so repositories can't be linked.
    let github = get("/api/github").await.json();
    assert!(github["account"].is_null() && github["repos"].as_array().unwrap().is_empty());
    let link = post("/api/repos", r#"{"repo":"acme/app"}"#).await;
    assert_eq!(link.status, 400);
    assert!(
        link.body.contains("not signed in to GitHub"),
        "{}",
        link.body
    );
    agent.shutdown().await;
}
