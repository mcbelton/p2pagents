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
    assert_eq!(done["target"], "this agent");

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

    let removed = request(port, "DELETE", "/api/squads/qa", &host, Some(token), None).await;
    assert_eq!(removed.status, 200);
    boss.shutdown().await;
    worker.shutdown().await;
}
