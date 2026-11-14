//! LLM agent backends: GitHub Copilot (official Rust SDK), Claude Agent SDK and
//! OpenAI Codex SDK (official TypeScript SDKs, reached through the Node bridge).

pub mod bridge;
pub mod copilot;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use bridge::Bridge;
pub use copilot::CopilotBackend;

use crate::auth::ProviderTools;
use crate::profile::Profile;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Copilot,
    Claude,
    Codex,
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(match self {
            Provider::Copilot => "copilot",
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        })
    }
}

/// What the executing agent lets an LLM do inside its workspace. Always decided
/// by the agent that runs the task, never by the peer that requested it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    /// Read files and search only.
    #[default]
    ReadOnly,
    /// Read, edit files and run shell commands in the workspace (coding tasks).
    WorkspaceWrite,
}

/// A unit of LLM work, local or delegated to a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmTask {
    pub provider: Provider,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl LlmTask {
    pub fn new(provider: Provider, prompt: impl Into<String>) -> Self {
        Self {
            provider,
            prompt: prompt.into(),
            model: None,
        }
    }
}

/// Runs [`LlmTask`]s. Implemented by [`SdkExecutor`]; tests can supply fakes.
#[async_trait]
pub trait Executor: Send + Sync {
    fn providers(&self) -> Vec<Provider>;
    async fn execute(&self, task: &LlmTask) -> Result<String>;
}

pub struct ExecutorConfig {
    pub providers: Vec<Provider>,
    pub workspace: PathBuf,
    pub access: Access,
    /// Path to `bridge/index.mjs`; needed for Claude and Codex.
    pub bridge_script: PathBuf,
    /// Identity whose private provider homes and credentials this agent uses.
    pub profile: Profile,
}

/// Executes tasks with the official Copilot, Claude Agent and Codex SDKs, using
/// provider instances private to this agent's profile.
pub struct SdkExecutor {
    providers: Vec<Provider>,
    workspace: PathBuf,
    access: Access,
    copilot: Option<CopilotBackend>,
    bridge: Option<Bridge>,
    accounts: Vec<(Provider, String)>,
}

impl SdkExecutor {
    /// Starts this agent's own provider instances and verifies each is signed in;
    /// fails (with the `auth login` command to run) instead of using another account.
    pub async fn start(config: ExecutorConfig) -> Result<Arc<Self>> {
        let mut providers = config.providers;
        providers.sort();
        providers.dedup();
        let workspace = std::path::absolute(&config.workspace)?;
        if !workspace.is_dir() {
            bail!("workspace {} is not a directory", workspace.display());
        }
        let profile = &config.profile;
        let tools = ProviderTools::new(&config.bridge_script);
        let mut accounts = Vec::new();

        let copilot = if providers.contains(&Provider::Copilot) {
            let backend = CopilotBackend::start(
                profile.copilot_credentials()?,
                workspace.clone(),
                config.access,
            )
            .await?;
            accounts.push((Provider::Copilot, backend.account().await?));
            Some(backend)
        } else {
            None
        };
        let bridged: Vec<Provider> = providers
            .iter()
            .copied()
            .filter(|p| matches!(p, Provider::Claude | Provider::Codex))
            .collect();
        let bridge = if bridged.is_empty() {
            None
        } else {
            for &provider in &bridged {
                accounts.push((provider, tools.check(profile, provider).await?));
            }
            Some(Bridge::spawn_node(
                &config.bridge_script,
                profile.bridge_env(&bridged)?,
            )?)
        };
        Ok(Arc::new(Self {
            providers,
            workspace,
            access: config.access,
            copilot,
            bridge,
            accounts,
        }))
    }

    /// Account each enabled provider is signed in as.
    pub fn accounts(&self) -> &[(Provider, String)] {
        &self.accounts
    }
}

#[async_trait]
impl Executor for SdkExecutor {
    fn providers(&self) -> Vec<Provider> {
        self.providers.clone()
    }

    async fn execute(&self, task: &LlmTask) -> Result<String> {
        if !self.providers.contains(&task.provider) {
            bail!("provider {} is not enabled on this agent", task.provider);
        }
        match task.provider {
            Provider::Copilot => self.copilot.as_ref().expect("enabled").run(task).await,
            Provider::Claude | Provider::Codex => {
                self.bridge
                    .as_ref()
                    .expect("enabled")
                    .run(task, &self.workspace, self.access)
                    .await
            }
        }
    }
}
