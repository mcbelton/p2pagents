use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use github_copilot_sdk::handler::{PermissionHandler, PermissionResult};
use github_copilot_sdk::rpc::SandboxConfig;
use github_copilot_sdk::session::Session;
use github_copilot_sdk::types::{
    McpHttpServerConfig, McpServerConfig, McpStdioServerConfig, MessageOptions,
    PermissionRequestData, PermissionRequestKind, RequestId, SessionConfig, SessionId,
};
use github_copilot_sdk::{Client, ClientOptions};
use serde_json::{Value, json};

use super::policy::{Workspace, home_secret_paths};
use super::{Access, LlmTask};
use crate::mcp::McpServer;
use crate::profile::{
    CopilotCredentials, DEFAULT_GITHUB_HOST, INHERITED_CREDENTIAL_VARS, github_login,
};

const TASK_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// Built-in Copilot CLI tools a session may use, per access level. Everything
/// else (web_fetch, sub-agents, skills, MCP servers such as GitHub's) is not
/// offered to the model at all.
const READ_TOOLS: &[&str] = &["view", "glob", "grep"];
const WRITE_TOOLS: &[&str] = &["create", "edit"];
const SHELL_TOOLS: &[&str] = &[
    "powershell",
    "read_powershell",
    "write_powershell",
    "stop_powershell",
    "list_powershell",
    "bash",
    "read_bash",
    "write_bash",
    "stop_bash",
    "list_bash",
];

pub(crate) fn available_tools(access: Access) -> Vec<&'static str> {
    let mut tools = READ_TOOLS.to_vec();
    if access >= Access::WorkspaceWrite {
        tools.extend(WRITE_TOOLS);
    }
    if access >= Access::Full {
        tools.extend(SHELL_TOOLS);
    }
    tools
}

/// Whether this host can run the Copilot CLI's OS sandbox for shell commands.
#[derive(Debug, Clone)]
enum SandboxSupport {
    Supported {
        /// The backend can enforce `deniedPaths` (Linux/macOS, and Windows with a
        /// recent Process Security Environment).
        denied_paths: bool,
    },
    Unsupported(String),
    /// The Copilot CLI is too old to report it.
    Unknown,
}

impl SandboxSupport {
    fn denied_paths(&self) -> bool {
        match self {
            SandboxSupport::Supported { denied_paths } => *denied_paths,
            // Older Windows backends refuse to run any command under a policy with
            // denied paths; Bubblewrap and Seatbelt always enforce them.
            _ => !cfg!(windows),
        }
    }
}

/// OS sandbox for shell commands at `full` access: Windows ProcessContainer,
/// Linux Bubblewrap or macOS Seatbelt. Commands may write only in the workspace
/// (plus temp and tool caches), have no network, can't ask to run outside it, and
/// (where the backend enforces it) can't read the home folder's credential stores.
pub(crate) fn sandbox_config(deny_secrets: bool) -> SandboxConfig {
    let mut policy = json!({
        "network": { "allowOutbound": false, "allowLocalNetwork": false },
    });
    if deny_secrets {
        policy["filesystem"] = json!({ "deniedPaths": home_secret_paths() });
    }
    serde_json::from_value(json!({
        "enabled": true,
        "allowBypass": false,
        "addCurrentWorkingDirectory": true,
        "userPolicy": policy,
    }))
    .expect("valid sandbox config")
}

/// A private GitHub Copilot CLI instance (official `github-copilot-sdk` crate) for one
/// agent: its own `COPILOT_HOME`, its own token, and no fallback to the machine's
/// signed-in user or inherited `GH_TOKEN`-style variables.
pub struct CopilotBackend {
    client: Client,
    login: Option<String>,
    sandbox: SandboxSupport,
}

impl CopilotBackend {
    pub async fn start(creds: CopilotCredentials) -> Result<Self> {
        let cli = find_copilot_cli()?;
        let mut env = vec![("COPILOT_HOME".to_owned(), creds.home.into_os_string())];
        if creds.host != DEFAULT_GITHUB_HOST {
            env.push(("COPILOT_GH_HOST".to_owned(), creds.host.clone().into()));
        }
        let remove: Vec<&str> = INHERITED_CREDENTIAL_VARS
            .iter()
            .copied()
            .filter(|var| !env.iter().any(|(set, _)| set == var))
            .collect();
        let options = ClientOptions::default()
            .with_program(cli.clone())
            .with_env_remove(remove)
            .with_env(env)
            .with_github_token(creds.token.clone())
            .with_use_logged_in_user(false);
        let client = Client::start(options)
            .await
            .with_context(|| format!("starting Copilot CLI {}", cli.display()))?;
        let login = github_login(&creds.token, &creds.host);
        let sandbox = match client.rpc().sandbox().get_host_support().await {
            Ok(support) if support.supported => SandboxSupport::Supported {
                denied_paths: support
                    .capabilities
                    .iter()
                    .any(|c| c.name == "denied_paths" && c.supported),
            },
            Ok(support) => SandboxSupport::Unsupported(
                support
                    .reason
                    .unwrap_or_else(|| "no sandbox backend on this host".into()),
            ),
            Err(_) => SandboxSupport::Unknown,
        };
        Ok(Self {
            client,
            login,
            sandbox,
        })
    }

    /// `login@host` this instance is authenticated as; fails if Copilot rejects the token.
    pub async fn account(&self) -> Result<String> {
        let status = self
            .client
            .get_auth_status()
            .await
            .context("querying Copilot auth status")?;
        if !status.is_authenticated {
            bail!(
                "Copilot rejected this profile's token: {}",
                status
                    .status_message
                    .unwrap_or_else(|| "not authenticated".into())
            );
        }
        let host = status.host.unwrap_or_default();
        let host = host.trim_start_matches("https://").trim_end_matches('/');
        let login = status.login.or_else(|| self.login.clone());
        Ok(format!(
            "{}@{host}",
            login.as_deref().unwrap_or("<unknown login>")
        ))
    }

    pub async fn stop(&self) {
        let _ = self.client.stop().await;
    }

    /// Runs `task` in `workspace` with `access` (tools, permissions and sandbox follow
    /// it) and the MCP servers `mcp`.
    pub async fn run(
        &self,
        task: &LlmTask,
        access: Access,
        workspace: &Path,
        mcp: &BTreeMap<String, McpServer>,
    ) -> Result<String> {
        let access = match (&self.sandbox, access) {
            (SandboxSupport::Unsupported(reason), Access::Full) => {
                eprintln!(
                    "  copilot: no OS sandbox on this host ({reason}); running without shell commands"
                );
                Access::WorkspaceWrite
            }
            _ => access,
        };
        let policy = AccessPolicy {
            access,
            workspace: Workspace::new(workspace)
                .with_context(|| format!("opening workspace {}", workspace.display()))?,
            mcp: mcp.clone(),
        };
        let mut tools: Vec<String> = available_tools(access)
            .into_iter()
            .map(String::from)
            .collect();
        tools.extend(mcp_tool_allowlist(mcp));
        let mut config = SessionConfig::default()
            .with_working_directory(workspace.to_path_buf())
            .with_available_tools(tools)
            .with_permission_handler(Arc::new(policy));
        if !mcp.is_empty() {
            config = config.with_mcp_servers(mcp_servers(mcp));
        }
        if access == Access::Full {
            config = config.with_sandbox_config(sandbox_config(self.sandbox.denied_paths()));
        }
        if let Some(model) = &task.model {
            config = config.with_model(model.clone());
        }
        let session = Arc::new(
            self.client
                .create_session(config)
                .await
                .context("creating Copilot session")?,
        );
        // If this future is dropped (the task was cancelled), stop the model too.
        let mut guard = AbortOnDrop(Some(session.clone()));
        let reply = session
            .send_and_wait(MessageOptions::new(task.prompt.clone()).with_wait_timeout(TASK_TIMEOUT))
            .await;
        guard.0 = None;
        let _ = session.disconnect().await;
        let event = reply.context("Copilot task failed")?;
        event
            .as_ref()
            .and_then(|e| e.data.get("content"))
            .and_then(|c| c.as_str())
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("Copilot returned no assistant message"))
    }
}

/// Aborts and disconnects a Copilot session whose task future was dropped.
struct AbortOnDrop(Option<Arc<Session>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        let Some(session) = self.0.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = session.abort().await;
                let _ = session.disconnect().await;
                eprintln!("  copilot: task cancelled, session aborted");
            });
        }
    }
}

/// `COPILOT_CLI_PATH`, else `copilot` on `PATH` (the SDK itself does not search `PATH`).
pub fn find_copilot_cli() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("COPILOT_CLI_PATH") {
        return Ok(PathBuf::from(path));
    }
    let names: &[&str] = if cfg!(windows) {
        &["copilot.exe", "copilot.cmd"]
    } else {
        &["copilot"]
    };
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("Copilot CLI not found; install it or set COPILOT_CLI_PATH"))
}

/// Copilot's name for the task's MCP server `name`. Copilot names MCP tools
/// `<server>-<tool>` and a bare server name in the tool allowlist offers all of its
/// tools, so the prefix keeps a server from ever being taken for a built-in tool.
fn session_server(name: &str) -> String {
    format!("mcp-{name}")
}

/// The task's MCP tools for Copilot's tool allowlist: each allowed tool, or the
/// whole server when the owner allows all of its tools.
pub(crate) fn mcp_tool_allowlist(mcp: &BTreeMap<String, McpServer>) -> Vec<String> {
    mcp.iter()
        .flat_map(|(name, s)| {
            let server = session_server(name);
            if s.tools.is_empty() {
                vec![server]
            } else {
                s.tools.iter().map(|t| format!("{server}-{t}")).collect()
            }
        })
        .collect()
}

/// The task's MCP servers in the Copilot SDK's form; each exposes only its allowed tools.
pub(crate) fn mcp_servers(
    mcp: &BTreeMap<String, McpServer>,
) -> github_copilot_sdk::IndexMap<String, McpServerConfig> {
    mcp.iter()
        .filter_map(|(name, s)| {
            let tools = Some(if s.tools.is_empty() {
                vec!["*".to_owned()]
            } else {
                s.tools.clone()
            });
            let config = match (&s.command, &s.url) {
                (Some(command), _) => McpServerConfig::Stdio(McpStdioServerConfig {
                    tools,
                    command: command.clone(),
                    args: s.args.clone(),
                    env: s.env.clone().into_iter().collect(),
                    ..Default::default()
                }),
                (_, Some(url)) => {
                    let http = McpHttpServerConfig {
                        tools,
                        url: url.clone(),
                        headers: s.headers.clone().into_iter().collect(),
                        ..Default::default()
                    };
                    if s.sse {
                        McpServerConfig::Sse(http)
                    } else {
                        McpServerConfig::Http(http)
                    }
                }
                _ => return None,
            };
            Some((session_server(name), config))
        })
        .collect()
}

struct AccessPolicy {
    access: Access,
    workspace: Workspace,
    mcp: BTreeMap<String, McpServer>,
}

/// Why Copilot's MCP tool call `req` is refused, or `None` to approve: only the
/// task's servers, and only the tools the owner allows on each.
pub(crate) fn mcp_refusal(mcp: &BTreeMap<String, McpServer>, req: &Value) -> Option<String> {
    if mcp.is_empty() {
        return Some("MCP tools are disabled for tasks".into());
    }
    let session_name = req["serverName"].as_str().unwrap_or_default();
    let name = req["toolName"].as_str().unwrap_or_default();
    let tool = name
        .strip_prefix(session_name)
        .and_then(|t| t.strip_prefix('-'))
        .unwrap_or(name);
    let server = session_name.strip_prefix("mcp-").unwrap_or_default();
    match mcp.get(server) {
        None => Some(format!(
            "{session_name:?} is not an MCP server this task may use"
        )),
        Some(s) if !s.allows_tool(tool) => Some(format!(
            "{tool:?} is not one of the tools the owner allows on MCP server {server}"
        )),
        Some(_) => None,
    }
}

/// String values of `key` in a permission request (string, array or map values).
fn strings(req: &Value, key: &str) -> Vec<String> {
    match req.get(key) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Some(Value::Object(map)) => map
            .values()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// Why Copilot's permission request `req` is refused at `access`, or `None` to approve.
pub(crate) fn refusal(
    access: Access,
    workspace: &Workspace,
    kind: Option<&PermissionRequestKind>,
    req: &Value,
) -> Option<String> {
    use PermissionRequestKind::*;
    if req["requestSandboxBypass"].as_bool() == Some(true) {
        return Some("tasks may not run outside the sandbox".into());
    }
    match kind {
        Some(Read) => {
            let paths = [strings(req, "path"), strings(req, "resolvedPath")].concat();
            if paths.is_empty() {
                return Some("read request without a path".into());
            }
            paths.iter().find_map(|p| workspace.check_read(p))
        }
        Some(Write) if access >= Access::WorkspaceWrite => {
            let mut paths = [strings(req, "fileName"), strings(req, "resolvedPath")].concat();
            for edit in req
                .get("fileEdits")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                for side in ["before", "after"] {
                    if let Some(path) = edit[side]["path"].as_str() {
                        paths.push(path.to_owned());
                    }
                }
            }
            if paths.is_empty() {
                return Some("write request without a path".into());
            }
            paths.iter().find_map(|p| workspace.check_write(p))
        }
        // Best effort only: a shell command can do anything the OS user can.
        Some(Shell) if access >= Access::Full => {
            if req["possibleUrls"]
                .as_array()
                .is_some_and(|urls| !urls.is_empty())
            {
                return Some("shell commands may not access URLs".into());
            }
            let text = strings(req, "fullCommandText");
            if text.is_empty() {
                return Some("shell request without a command".into());
            }
            text.iter()
                .find_map(|t| workspace.check_command(t))
                .or_else(|| {
                    [
                        strings(req, "possiblePaths"),
                        strings(req, "resolvedPaths"),
                        strings(req, "resolvedWorkingDirectory"),
                    ]
                    .concat()
                    .iter()
                    .find_map(|p| workspace.check_read(p))
                })
        }
        Some(Write) => Some(format!("{access} access does not allow editing files")),
        Some(Shell) => Some(format!("{access} access does not allow shell commands")),
        Some(Url) => Some("web access is disabled for tasks".into()),
        Some(Mcp) => Some("MCP tools are disabled for tasks".into()),
        _ => Some("this tool is disabled for tasks".into()),
    }
}

#[async_trait]
impl PermissionHandler for AccessPolicy {
    async fn handle(
        &self,
        _sid: SessionId,
        _rid: RequestId,
        data: PermissionRequestData,
    ) -> PermissionResult {
        let req = Value::from(&data);
        if std::env::var_os("P2PAGENTS_DEBUG_PERMISSIONS").is_some() {
            eprintln!("  copilot: permission request {req}");
        }
        let reason = if data.managed_approval_required == Some(true) {
            Some("managed policy requires a human decision".into())
        } else if data.kind == Some(PermissionRequestKind::Mcp) {
            mcp_refusal(&self.mcp, &req)
        } else {
            refusal(self.access, &self.workspace, data.kind.as_ref(), &req)
        };
        match reason {
            None => PermissionResult::approve_once(),
            Some(reason) => {
                eprintln!("  copilot: denied {} ({reason})", short(&req));
                PermissionResult::reject(Some(format!("denied by p2pagents: {reason}")))
            }
        }
    }
}

/// One-line summary of a permission request for the agent's log.
fn short(req: &Value) -> String {
    let kind = req["kind"].as_str().unwrap_or("?");
    let target = ["path", "fileName", "fullCommandText", "url", "toolName"]
        .iter()
        .find_map(|k| req[*k].as_str())
        .unwrap_or("");
    let target: String = target.chars().take(120).collect();
    format!("{kind} {target}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use PermissionRequestKind::*;
    use serde_json::json;

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        (dir, ws)
    }

    fn file(ws: &Workspace, rel: &str) -> String {
        ws.root().join(rel).to_string_lossy().into_owned()
    }

    #[test]
    fn tools_grow_with_access() {
        let read = available_tools(Access::ReadOnly);
        assert_eq!(read, ["view", "glob", "grep"]);
        let write = available_tools(Access::WorkspaceWrite);
        assert!(write.contains(&"edit") && !write.contains(&"powershell"));
        let full = available_tools(Access::Full);
        assert!(full.contains(&"powershell") && full.contains(&"bash"));
        for access in [Access::ReadOnly, Access::WorkspaceWrite, Access::Full] {
            for tool in ["web_fetch", "task", "skill", "write_agent"] {
                assert!(!available_tools(access).contains(&tool));
            }
        }
    }

    #[test]
    fn reads_are_confined_to_the_workspace() {
        let (_dir, ws) = workspace();
        let read = |path: &str| json!({ "kind": "read", "path": path });
        let inside = read(&file(&ws, "src/main.rs"));
        assert_eq!(refusal(Access::ReadOnly, &ws, Some(&Read), &inside), None);
        let outside = std::env::temp_dir().join("elsewhere.txt");
        let outside = read(outside.to_str().unwrap());
        assert!(refusal(Access::Full, &ws, Some(&Read), &outside).is_some());
        let secret = read(&file(&ws, ".env"));
        assert!(refusal(Access::Full, &ws, Some(&Read), &secret).is_some());
        assert!(
            refusal(
                Access::ReadOnly,
                &ws,
                Some(&Read),
                &json!({ "kind": "read" })
            )
            .is_some()
        );
    }

    #[test]
    fn writes_need_write_access_and_stay_in_the_workspace() {
        let (_dir, ws) = workspace();
        let write = |path: &str| json!({ "kind": "write", "fileName": path });
        let ok = write(&file(&ws, "src/lib.rs"));
        assert!(refusal(Access::ReadOnly, &ws, Some(&Write), &ok).is_some());
        assert_eq!(
            refusal(Access::WorkspaceWrite, &ws, Some(&Write), &ok),
            None
        );
        let hook = write(&file(&ws, ".git/hooks/pre-commit"));
        assert!(refusal(Access::Full, &ws, Some(&Write), &hook).is_some());
        let sneaky = json!({
            "kind": "write",
            "fileName": file(&ws, "a.txt"),
            "fileEdits": [{ "after": { "path": "../escape.txt", "content": "" } }],
        });
        assert!(refusal(Access::Full, &ws, Some(&Write), &sneaky).is_some());
    }

    #[test]
    fn shell_needs_full_access_and_no_urls_or_outside_paths() {
        let (_dir, ws) = workspace();
        let cmd = |paths: Vec<String>, urls: Value| json!({ "kind": "shell", "fullCommandText": "x", "possiblePaths": paths, "possibleUrls": urls });
        let plain = cmd(vec![file(&ws, "Cargo.toml")], json!([]));
        assert!(refusal(Access::WorkspaceWrite, &ws, Some(&Shell), &plain).is_some());
        assert_eq!(refusal(Access::Full, &ws, Some(&Shell), &plain), None);
        let curl = cmd(vec![], json!([{ "url": "https://example.com" }]));
        assert!(refusal(Access::Full, &ws, Some(&Shell), &curl).is_some());
        let home = std::env::temp_dir().to_string_lossy().into_owned();
        let outside = cmd(vec![home], json!([]));
        assert!(refusal(Access::Full, &ws, Some(&Shell), &outside).is_some());
    }

    #[test]
    fn network_mcp_and_other_tools_are_always_refused() {
        let (_dir, ws) = workspace();
        for kind in [Url, Mcp, CustomTool, Memory, Hook, Unknown] {
            assert!(refusal(Access::Full, &ws, Some(&kind), &json!({})).is_some());
        }
        assert!(refusal(Access::Full, &ws, None, &json!({})).is_some());
    }

    #[test]
    fn mcp_calls_only_reach_the_tasks_servers_and_allowed_tools() {
        let mut browser = McpServer::local("npx", vec!["@playwright/mcp".into()]);
        browser.tools = vec!["browser_snapshot".into()];
        browser.env.insert("TOKEN".into(), "t".into());
        let docs = McpServer::remote("https://docs.example.com/mcp", true);
        let mcp = BTreeMap::from([("browser".to_owned(), browser), ("docs".to_owned(), docs)]);
        // As Copilot sends them: the session's server name and `<server>-<tool>`.
        let call = |server: &str, tool: &str| json!({ "kind": "mcp", "serverName": format!("mcp-{server}"), "toolName": format!("mcp-{server}-{tool}"), "toolTitle": tool });
        assert_eq!(
            mcp_refusal(&mcp, &call("browser", "browser_snapshot")),
            None
        );
        assert_eq!(mcp_refusal(&mcp, &call("docs", "search")), None);
        assert!(mcp_refusal(&mcp, &call("browser", "browser_evaluate")).is_some());
        assert!(mcp_refusal(&mcp, &call("github", "get_me")).is_some());
        let unprefixed = json!({ "kind": "mcp", "serverName": "docs", "toolName": "docs-search" });
        assert!(
            mcp_refusal(&mcp, &unprefixed).is_some(),
            "not a server of this task"
        );
        assert!(mcp_refusal(&mcp, &json!({ "kind": "mcp" })).is_some());
        assert!(mcp_refusal(&BTreeMap::new(), &call("docs", "search")).is_some());

        assert_eq!(
            mcp_tool_allowlist(&mcp),
            ["mcp-browser-browser_snapshot", "mcp-docs"],
            "one tool, or all of a server's tools; never a built-in tool's name"
        );
        let config = serde_json::to_value(mcp_servers(&mcp)).unwrap();
        assert_eq!(config["mcp-browser"]["type"], "stdio");
        assert_eq!(config["mcp-browser"]["tools"], json!(["browser_snapshot"]));
        assert_eq!(config["mcp-browser"]["env"]["TOKEN"], "t");
        assert_eq!(config["mcp-docs"]["type"], "sse");
        assert_eq!(config["mcp-docs"]["tools"], json!(["*"]));
    }

    #[test]
    fn shell_runs_in_an_offline_sandbox_without_bypass() {
        let config = serde_json::to_value(sandbox_config(true)).unwrap();
        assert_eq!(config["enabled"], true);
        assert_eq!(config["allowBypass"], false);
        assert_eq!(config["userPolicy"]["network"]["allowOutbound"], false);
        assert_eq!(config["userPolicy"]["network"]["allowLocalNetwork"], false);
        assert!(config["userPolicy"]["filesystem"]["deniedPaths"].is_array());
        let without = serde_json::to_value(sandbox_config(false)).unwrap();
        assert!(without["userPolicy"]["filesystem"].is_null());

        let supported = SandboxSupport::Supported { denied_paths: true };
        assert!(supported.denied_paths());
        let old_windows = SandboxSupport::Supported {
            denied_paths: false,
        };
        assert!(!old_windows.denied_paths());
        assert_eq!(SandboxSupport::Unknown.denied_paths(), !cfg!(windows));

        let (_dir, ws) = workspace();
        let bypass = json!({
            "kind": "shell",
            "fullCommandText": "cargo test",
            "requestSandboxBypass": true,
        });
        assert!(refusal(Access::Full, &ws, Some(&Shell), &bypass).is_some());
    }
}
