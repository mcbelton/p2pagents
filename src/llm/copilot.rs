use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use github_copilot_sdk::handler::{PermissionHandler, PermissionResult};
use github_copilot_sdk::types::{
    MessageOptions, PermissionRequestData, PermissionRequestKind, RequestId, SessionConfig,
    SessionId,
};
use github_copilot_sdk::{Client, ClientOptions};

use super::{Access, LlmTask};
use crate::profile::{
    CopilotCredentials, DEFAULT_GITHUB_HOST, INHERITED_CREDENTIAL_VARS, github_login,
};

const TASK_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// A private GitHub Copilot CLI instance (official `github-copilot-sdk` crate) for one
/// agent: its own `COPILOT_HOME`, its own token, and no fallback to the machine's
/// signed-in user or inherited `GH_TOKEN`-style variables.
pub struct CopilotBackend {
    workspace: PathBuf,
    access: Access,
    client: Client,
    login: Option<String>,
}

impl CopilotBackend {
    pub async fn start(
        creds: CopilotCredentials,
        workspace: PathBuf,
        access: Access,
    ) -> Result<Self> {
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
        Ok(Self {
            workspace,
            access,
            client,
            login,
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

    pub async fn run(&self, task: &LlmTask) -> Result<String> {
        let mut config = SessionConfig::default()
            .with_working_directory(self.workspace.clone())
            .with_permission_handler(Arc::new(AccessPolicy(self.access)));
        if let Some(model) = &task.model {
            config = config.with_model(model.clone());
        }
        let session = self
            .client
            .create_session(config)
            .await
            .context("creating Copilot session")?;
        let reply = session
            .send_and_wait(MessageOptions::new(task.prompt.clone()).with_wait_timeout(TASK_TIMEOUT))
            .await;
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

struct AccessPolicy(Access);

pub(crate) fn is_allowed(access: Access, kind: Option<&PermissionRequestKind>) -> bool {
    use PermissionRequestKind::*;
    matches!(
        (access, kind),
        (_, Some(Read)) | (Access::WorkspaceWrite, Some(Write | Shell))
    )
}

#[async_trait]
impl PermissionHandler for AccessPolicy {
    async fn handle(
        &self,
        _sid: SessionId,
        _rid: RequestId,
        data: PermissionRequestData,
    ) -> PermissionResult {
        if data.managed_approval_required != Some(true) && is_allowed(self.0, data.kind.as_ref()) {
            PermissionResult::approve_once()
        } else {
            PermissionResult::reject(Some(format!("denied by p2pagents {:?} policy", self.0)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PermissionRequestKind::*;

    #[test]
    fn read_only_allows_only_reads() {
        assert!(is_allowed(Access::ReadOnly, Some(&Read)));
        for kind in [Write, Shell, Url, Mcp] {
            assert!(!is_allowed(Access::ReadOnly, Some(&kind)));
        }
        assert!(!is_allowed(Access::ReadOnly, None));
    }

    #[test]
    fn workspace_write_allows_coding_but_not_network_or_mcp() {
        for kind in [Read, Write, Shell] {
            assert!(is_allowed(Access::WorkspaceWrite, Some(&kind)));
        }
        for kind in [Url, Mcp, CustomTool, Memory, Hook, Unknown] {
            assert!(!is_allowed(Access::WorkspaceWrite, Some(&kind)));
        }
    }
}
