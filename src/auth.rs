//! Signing a profile in and out of each provider, and checking which account it uses.
//! Every command runs against the profile's own config home with inherited
//! credential variables removed, so it never touches the machine-wide sign-in.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::llm::{CopilotBackend, Provider};
use crate::profile::{
    ClaudeAuth, CodexAuth, CopilotAuth, INHERITED_CREDENTIAL_VARS, Profile, gh_token,
};

/// Keeps Codex credentials in the profile's `CODEX_HOME/auth.json`, not the OS keyring.
pub const CODEX_FILE_STORE: &str = r#"cli_auth_credentials_store="file""#;

/// How to sign a profile in to a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginMethod {
    /// Interactive browser sign-in (Claude, Codex).
    Browser,
    /// Copilot: use the token of a GitHub CLI account (`gh auth login`).
    GhUser { user: String, host: String },
    /// Copilot: a GitHub token; Claude: an OAuth token from `claude setup-token`.
    Token { host: String },
    /// Claude / Codex: an API key.
    ApiKey,
}

/// Locates the provider CLIs bundled with the bridge's npm packages.
pub struct ProviderTools {
    bridge_dir: PathBuf,
}

impl ProviderTools {
    pub fn new(bridge_script: &Path) -> Self {
        let bridge_dir = bridge_script
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Self { bridge_dir }
    }

    fn isolated(mut cmd: Command) -> Command {
        for var in INHERITED_CREDENTIAL_VARS {
            cmd.env_remove(var);
        }
        cmd
    }

    pub fn claude(&self, profile: &Profile) -> Result<Command> {
        let scope = self.bridge_dir.join("node_modules").join("@anthropic-ai");
        let exe = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        let path = std::fs::read_dir(&scope)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("claude-agent-sdk-")
            })
            .map(|e| e.path().join(exe))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                anyhow!(
                    "bundled Claude CLI not found under {}; run npm install in bridge/",
                    scope.display()
                )
            })?;
        let mut cmd = Self::isolated(Command::new(path));
        cmd.env("CLAUDE_CONFIG_DIR", profile.home(Provider::Claude)?);
        Ok(cmd)
    }

    pub fn codex(&self, profile: &Profile) -> Result<Command> {
        let script = self
            .bridge_dir
            .join("node_modules/@openai/codex/bin/codex.js");
        if !script.is_file() {
            bail!(
                "bundled Codex CLI not found at {}; run npm install in bridge/",
                script.display()
            );
        }
        let mut cmd = Self::isolated(Command::new("node"));
        cmd.arg(script).args(["-c", CODEX_FILE_STORE]);
        cmd.env("CODEX_HOME", profile.home(Provider::Codex)?);
        Ok(cmd)
    }

    /// Verifies that `profile` can use `provider` and describes the account.
    pub async fn check(&self, profile: &Profile, provider: Provider) -> Result<String> {
        match provider {
            Provider::Copilot => {
                let creds = profile.copilot_credentials()?;
                let backend = CopilotBackend::start(creds, std::env::temp_dir()).await?;
                let account = backend.account().await;
                backend.stop().await;
                let source = match &profile.config.copilot {
                    Some(CopilotAuth::Gh { .. }) => "token from gh",
                    _ => "stored token",
                };
                Ok(format!("{} ({source})", account?))
            }
            Provider::Claude => match profile.config.claude {
                None => Err(profile.not_signed_in(provider)),
                Some(ClaudeAuth::ApiKey) => profile
                    .secret(provider)
                    .map(|_| "Anthropic API key (stored)".into()),
                Some(ClaudeAuth::OauthToken) => profile
                    .secret(provider)
                    .map(|_| "Claude OAuth token (stored)".into()),
                Some(ClaudeAuth::Login) => {
                    let out = self
                        .claude(profile)?
                        .args(["auth", "status", "--json"])
                        .output()?;
                    let status: ClaudeStatus =
                        serde_json::from_slice(&out.stdout).with_context(|| {
                            format!(
                                "unexpected `claude auth status` output: {}",
                                String::from_utf8_lossy(&out.stdout)
                            )
                        })?;
                    if !status.logged_in {
                        return Err(profile.not_signed_in(provider));
                    }
                    Ok(status
                        .email
                        .or(status.auth_method)
                        .unwrap_or_else(|| "signed in".into()))
                }
            },
            Provider::Codex => {
                if profile.config.codex.is_none() {
                    return Err(profile.not_signed_in(provider));
                }
                let out = self.codex(profile)?.args(["login", "status"]).output()?;
                if !out.status.success() {
                    return Err(profile.not_signed_in(provider));
                }
                // Codex prints its status on stderr, after possible warnings.
                let text = [&out.stdout, &out.stderr]
                    .iter()
                    .flat_map(|b| {
                        String::from_utf8_lossy(b)
                            .lines()
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .find(|l| l.starts_with("Logged in"))
                    .unwrap_or_else(|| "signed in".into());
                Ok(text)
            }
        }
    }

    /// Signs `profile` in to `provider`, saves the profile and returns the verified account.
    pub async fn login(
        &self,
        profile: &mut Profile,
        provider: Provider,
        method: LoginMethod,
    ) -> Result<String> {
        let existed = profile.dir.join("profile.json").is_file();
        match (provider, method) {
            (Provider::Copilot, LoginMethod::GhUser { user, host }) => {
                gh_token(&user, &host)?;
                profile.delete_secret(provider)?;
                profile.config.copilot = Some(CopilotAuth::Gh { user, host });
            }
            (Provider::Copilot, LoginMethod::Token { host }) => {
                profile.set_secret(provider, &read_secret("GitHub token (fine-grained PAT with Copilot Requests, or gho_ OAuth token)")?)?;
                profile.config.copilot = Some(CopilotAuth::Token { host });
            }
            (Provider::Copilot, _) => bail!(
                "Copilot needs --gh-user <login> (an account signed in with `gh auth login`) or --token"
            ),
            (Provider::Claude, LoginMethod::Browser) => {
                run_interactive(self.claude(profile)?.args(["auth", "login"]))?;
                profile.delete_secret(provider)?;
                profile.config.claude = Some(ClaudeAuth::Login);
            }
            (Provider::Claude, LoginMethod::ApiKey) => {
                profile.set_secret(provider, &read_secret("Anthropic API key")?)?;
                profile.config.claude = Some(ClaudeAuth::ApiKey);
            }
            (Provider::Claude, LoginMethod::Token { .. }) => {
                profile.set_secret(
                    provider,
                    &read_secret("Claude OAuth token (from `claude setup-token`)")?,
                )?;
                profile.config.claude = Some(ClaudeAuth::OauthToken);
            }
            (Provider::Claude, LoginMethod::GhUser { .. }) => {
                bail!("--gh-user only applies to copilot")
            }
            (Provider::Codex, LoginMethod::Browser) => {
                run_interactive(self.codex(profile)?.arg("login"))?;
                profile.config.codex = Some(CodexAuth::Login);
            }
            (Provider::Codex, LoginMethod::ApiKey) => {
                let key = read_secret("OpenAI API key")?;
                let mut child = self
                    .codex(profile)?
                    .args(["login", "--with-api-key"])
                    .stdin(Stdio::piped())
                    .spawn()?;
                child
                    .stdin
                    .take()
                    .expect("piped")
                    .write_all(key.as_bytes())?;
                if !child.wait()?.success() {
                    bail!("codex login --with-api-key failed");
                }
                profile.config.codex = Some(CodexAuth::Login);
            }
            (Provider::Codex, _) => bail!("Codex supports browser sign-in or --api-key"),
        }
        match self.check(profile, provider).await {
            Ok(account) => {
                profile.save()?;
                Ok(account)
            }
            Err(e) => {
                let _ = self.logout(profile, provider);
                if !existed {
                    let _ = std::fs::remove_dir_all(&profile.dir);
                }
                Err(e.context(format!(
                    "{provider} sign-in for profile '{}' failed; {provider} is signed out",
                    profile.name
                )))
            }
        }
    }

    pub fn logout(&self, profile: &mut Profile, provider: Provider) -> Result<()> {
        match provider {
            Provider::Copilot => profile.config.copilot = None,
            Provider::Claude => {
                if profile.config.claude == Some(ClaudeAuth::Login) {
                    let _ = self.claude(profile)?.args(["auth", "logout"]).status();
                }
                profile.config.claude = None;
            }
            Provider::Codex => {
                if profile.config.codex.is_some() {
                    let _ = self.codex(profile)?.arg("logout").status();
                }
                profile.config.codex = None;
            }
        }
        profile.delete_secret(provider)?;
        profile.save()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeStatus {
    logged_in: bool,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    auth_method: Option<String>,
}

fn run_interactive(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().context("starting the provider CLI")?;
    if !status.success() {
        bail!("sign-in did not complete ({status})");
    }
    Ok(())
}

/// Reads a secret from a hidden prompt, or from stdin when it is piped.
fn read_secret(what: &str) -> Result<String> {
    let secret = if std::io::stdin().is_terminal() {
        rpassword::prompt_password(format!("{what}: "))?
    } else {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    };
    let secret = secret.trim().to_owned();
    if secret.is_empty() {
        bail!("no {what} provided");
    }
    Ok(secret)
}
