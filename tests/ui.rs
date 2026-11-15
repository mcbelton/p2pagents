//! Web UI server: access token, Host check, security headers and the task API.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use p2pagents::ui::{self, UiInfo};
use p2pagents::{
    Access, Agent, AgentConfig, Executor, LlmTask, Network, Profile, Provider, SecretKey,
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

    async fn execute(&self, task: &LlmTask, _grant: Access) -> anyhow::Result<String> {
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
