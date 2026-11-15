//! Per-agent identities. Each profile has its own Copilot, Claude and Codex home
//! directories and its own credentials, so agents can be signed in to different
//! accounts and never fall back to the machine-wide sign-in.
//!
//! Layout: `<root>/profiles/<name>/{profile.json, copilot/, claude/, codex/}` where
//! `<root>` is `$P2PAGENTS_HOME` or `~/.p2pagents`. Secrets (GitHub tokens, Anthropic
//! keys) live in the OS credential store under service `p2pagents`, user `<name>/<provider>`.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::llm::Provider;

/// Inherited credential/config variables that must not leak into an agent's
/// provider processes; each agent sets its own instead.
pub const INHERITED_CREDENTIAL_VARS: &[&str] = &[
    "COPILOT_GITHUB_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "COPILOT_HOME",
    "COPILOT_GH_HOST",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CONFIG_DIR",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CODEX_HOME",
];

/// Where profile secrets (identity key, tokens, API keys) are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStore {
    /// The OS credential store (Windows Credential Manager, macOS Keychain,
    /// Linux Secret Service).
    Keyring,
    /// Owner-only files in `<profile>/secrets/`: used when no OS credential store is
    /// available (headless Linux, WSL, containers) or `P2PAGENTS_SECRET_STORE=file`.
    Files,
}

impl SecretStore {
    pub fn current() -> Self {
        match std::env::var("P2PAGENTS_SECRET_STORE").as_deref() {
            Ok("file") => Self::Files,
            Ok("keyring") => Self::Keyring,
            _ if keyring::Entry::store_status().is_ok() => Self::Keyring,
            _ => Self::Files,
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::Keyring => "OS credential store",
            Self::Files => "owner-only files in each profile's secrets folder",
        }
    }
}

/// Writes `value` to `path`, readable only by the current user (0600 on Unix,
/// inheriting the user-profile ACL on Windows).
fn write_private_file(path: &Path, value: &str) -> Result<()> {
    let dir = path.parent().expect("secret files live in a folder");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("writing {}", path.display()))?;
        file.write_all(value.as_bytes())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, value).with_context(|| format!("writing {}", path.display()))
    }
}

/// OS credential store service name. Profiles under a custom `P2PAGENTS_HOME` get
/// their own namespace so they never collide with the default profiles.
fn keyring_service() -> String {
    match std::env::var("P2PAGENTS_HOME") {
        Ok(root) if !root.is_empty() => format!("p2pagents:{root}"),
        _ => "p2pagents".to_owned(),
    }
}
pub const DEFAULT_GITHUB_HOST: &str = "github.com";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum CopilotAuth {
    /// Token of an account signed in to the GitHub CLI, fetched at startup.
    Gh { user: String, host: String },
    /// Token stored in the OS credential store.
    Token { host: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum ClaudeAuth {
    /// `claude auth login` inside this profile's Claude config dir.
    Login,
    /// Anthropic API key in the OS credential store.
    ApiKey,
    /// Long-lived OAuth token (`claude setup-token`) in the OS credential store.
    OauthToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum CodexAuth {
    /// `codex login` (ChatGPT or API key) inside this profile's Codex home.
    Login,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copilot: Option<CopilotAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<ClaudeAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<CodexAuth>,
}

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub dir: PathBuf,
    pub config: ProfileConfig,
}

/// Credentials for one agent's private Copilot CLI instance.
#[derive(Clone)]
pub struct CopilotCredentials {
    pub token: String,
    pub host: String,
    pub home: PathBuf,
}

impl fmt::Debug for CopilotCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopilotCredentials")
            .field("token", &"<redacted>")
            .field("host", &self.host)
            .field("home", &self.home)
            .finish()
    }
}

/// Root folder for profiles and the installed bridge: `$P2PAGENTS_HOME` or `~/.p2pagents`.
pub fn data_root() -> Result<PathBuf> {
    if let Some(root) = std::env::var_os("P2PAGENTS_HOME") {
        return Ok(PathBuf::from(root));
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("cannot find the home directory; set P2PAGENTS_HOME")?;
    Ok(PathBuf::from(home).join(".p2pagents"))
}

pub fn profiles_root() -> Result<PathBuf> {
    Ok(data_root()?.join("profiles"))
}

pub fn validate_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.');
    if !ok {
        bail!("invalid profile name {name:?}: use letters, digits, '-', '_' or '.'");
    }
    Ok(())
}

impl Profile {
    pub fn open(name: &str) -> Result<Self> {
        Self::open_in(&profiles_root()?, name)
    }

    pub fn open_in(root: &Path, name: &str) -> Result<Self> {
        validate_name(name)?;
        let dir = root.join(name);
        let file = dir.join("profile.json");
        let config = if file.is_file() {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?
        } else {
            ProfileConfig::default()
        };
        Ok(Self {
            name: name.to_owned(),
            dir,
            config,
        })
    }

    pub fn list() -> Result<Vec<String>> {
        let root = profiles_root()?;
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Ok(Vec::new());
        };
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path().join("profile.json").is_file() || e.path().join("peers.json").is_file()
            })
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort();
        Ok(names)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let file = self.dir.join("profile.json");
        std::fs::write(&file, serde_json::to_string_pretty(&self.config)? + "\n")
            .with_context(|| format!("writing {}", file.display()))
    }

    /// This profile's private config/state directory for `provider` (created on demand).
    pub fn home(&self, provider: Provider) -> Result<PathBuf> {
        let dir = self.dir.join(provider.to_string());
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(dir)
    }

    pub fn is_configured(&self, provider: Provider) -> bool {
        match provider {
            Provider::Copilot => self.config.copilot.is_some(),
            Provider::Claude => self.config.claude.is_some(),
            Provider::Codex => self.config.codex.is_some(),
        }
    }

    pub fn login_hint(&self, provider: Provider) -> String {
        format!("p2pagents auth login {provider} --profile {}", self.name)
    }

    pub fn not_signed_in(&self, provider: Provider) -> anyhow::Error {
        anyhow!(
            "{provider} is not signed in for profile '{}'; run: {}",
            self.name,
            self.login_hint(provider)
        )
    }

    fn secret_get(&self, key: &str) -> Result<Option<String>> {
        match SecretStore::current() {
            SecretStore::Keyring => match self.keyring_entry(key)?.get_password() {
                Ok(v) => Ok(Some(v)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(e).context("reading the OS credential store"),
            },
            SecretStore::Files => {
                let path = self.secret_file(key);
                match std::fs::read_to_string(&path) {
                    Ok(v) => Ok(Some(v.trim().to_owned())),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
                }
            }
        }
    }

    fn secret_set(&self, key: &str, value: &str) -> Result<()> {
        match SecretStore::current() {
            SecretStore::Keyring => self
                .keyring_entry(key)?
                .set_password(value)
                .context("saving to the OS credential store"),
            SecretStore::Files => write_private_file(&self.secret_file(key), value),
        }
    }

    fn secret_delete(&self, key: &str) -> Result<()> {
        match SecretStore::current() {
            SecretStore::Keyring => match self.keyring_entry(key)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e).context("deleting from the OS credential store"),
            },
            SecretStore::Files => match std::fs::remove_file(self.secret_file(key)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e).context("deleting a stored secret"),
            },
        }
    }

    fn keyring_entry(&self, key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&keyring_service(), &format!("{}/{key}", self.name))
            .context("opening the OS credential store")
    }

    fn secret_file(&self, key: &str) -> PathBuf {
        self.dir.join("secrets").join(key)
    }

    pub fn set_secret(&self, provider: Provider, secret: &str) -> Result<()> {
        self.secret_set(&provider.to_string(), secret)
    }

    pub fn secret(&self, provider: Provider) -> Result<String> {
        self.secret_get(&provider.to_string())?
            .ok_or_else(|| self.not_signed_in(provider))
    }

    pub fn delete_secret(&self, provider: Provider) -> Result<()> {
        self.secret_delete(&provider.to_string())
    }

    /// This profile's agent identity (ed25519 key), created on first use and
    /// kept in the secret store. Its public half is the endpoint id.
    pub fn identity(&self) -> Result<iroh::SecretKey> {
        match self.secret_get("identity")? {
            Some(hex) => hex.parse().map_err(|e| {
                anyhow!(
                    "stored identity for profile '{}' is invalid: {e}",
                    self.name
                )
            }),
            None => {
                let key = iroh::SecretKey::generate();
                self.secret_set("identity", &data_encoding::HEXLOWER.encode(&key.to_bytes()))
                    .context("saving the agent identity")?;
                Ok(key)
            }
        }
    }

    pub fn delete_identity(&self) -> Result<()> {
        self.secret_delete("identity")
    }

    /// Allowlist of peers this profile's agents talk to.
    pub fn trust_path(&self) -> PathBuf {
        self.dir.join("peers.json")
    }

    /// Log of tasks peers ran on this profile's agents (see [`crate::audit`]).
    pub fn audit_path(&self) -> PathBuf {
        self.dir.join("audit.jsonl")
    }

    /// Folders this profile shares with peers, besides the built-in data folder.
    pub fn shares_path(&self) -> PathBuf {
        self.dir.join("shares.json")
    }

    /// The profile's `.p2pdata` folder: the built-in read-only `data` share.
    /// `~/.p2pdata/<profile>`, or `$P2PAGENTS_HOME/.p2pdata/<profile>`.
    pub fn data_folder(&self) -> PathBuf {
        let root = self
            .dir
            .parent()
            .and_then(Path::parent)
            .unwrap_or(&self.dir);
        // ~/.p2pagents -> ~/.p2pdata; a custom P2PAGENTS_HOME keeps it inside.
        let base = if root.file_name().is_some_and(|n| n == ".p2pagents") {
            root.parent().unwrap_or(root)
        } else {
            root
        };
        base.join(".p2pdata").join(&self.name)
    }

    /// The folders this profile shares (always including `data`).
    pub fn shares(&self) -> Result<crate::shares::ShareStore> {
        crate::shares::ShareStore::load(&self.shares_path(), self.data_folder())
    }

    /// Deletes the profile: its identity, stored secrets and folder (including
    /// the provider homes and their sign-ins).
    pub fn delete(self) -> Result<()> {
        for provider in [Provider::Copilot, Provider::Claude, Provider::Codex] {
            self.delete_secret(provider)?;
        }
        self.delete_identity()?;
        if self.dir.exists() {
            std::fs::remove_dir_all(&self.dir)
                .with_context(|| format!("deleting {}", self.dir.display()))?;
        }
        Ok(())
    }

    pub fn trust(&self) -> Result<crate::trust::TrustStore> {
        crate::trust::TrustStore::load(&self.trust_path())
    }

    pub fn copilot_credentials(&self) -> Result<CopilotCredentials> {
        let (token, host) = match &self.config.copilot {
            None => return Err(self.not_signed_in(Provider::Copilot)),
            Some(CopilotAuth::Gh { user, host }) => (gh_token(user, host)?, host.clone()),
            Some(CopilotAuth::Token { host }) => (self.secret(Provider::Copilot)?, host.clone()),
        };
        Ok(CopilotCredentials {
            token,
            host,
            home: self.home(Provider::Copilot)?,
        })
    }

    /// Environment for this profile's Claude/Codex processes (the Node bridge):
    /// isolated config homes plus this profile's own secrets.
    pub fn bridge_env(&self, providers: &[Provider]) -> Result<Vec<(String, OsString)>> {
        let mut env = Vec::new();
        if providers.contains(&Provider::Claude) {
            env.push((
                "CLAUDE_CONFIG_DIR".into(),
                self.home(Provider::Claude)?.into_os_string(),
            ));
            match self.config.claude {
                None => return Err(self.not_signed_in(Provider::Claude)),
                Some(ClaudeAuth::Login) => {}
                Some(ClaudeAuth::ApiKey) => env.push((
                    "ANTHROPIC_API_KEY".into(),
                    self.secret(Provider::Claude)?.into(),
                )),
                Some(ClaudeAuth::OauthToken) => env.push((
                    "CLAUDE_CODE_OAUTH_TOKEN".into(),
                    self.secret(Provider::Claude)?.into(),
                )),
            }
        }
        if providers.contains(&Provider::Codex) {
            if self.config.codex.is_none() {
                return Err(self.not_signed_in(Provider::Codex));
            }
            env.push((
                "CODEX_HOME".into(),
                self.home(Provider::Codex)?.into_os_string(),
            ));
        }
        Ok(env)
    }
}

/// GitHub login that `token` belongs to, looked up with `curl` (token passed on
/// stdin, never on the command line). `None` if curl is unavailable or the call fails.
pub fn github_login(token: &str, host: &str) -> Option<String> {
    use std::io::Write;
    let api = if host == DEFAULT_GITHUB_HOST {
        "https://api.github.com/user".to_owned()
    } else {
        format!("https://api.{host}/user")
    };
    let mut child = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
        .args(["-sf", "-H", "@-", "-H", "User-Agent: p2pagents", &api])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    child
        .stdin
        .take()?
        .write_all(format!("Authorization: Bearer {token}\n").as_bytes())
        .ok()?;
    let out = child.wait_with_output().ok()?;
    let user: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    user.get("login")?.as_str().map(str::to_owned)
}

/// Token for `user@host` from the GitHub CLI (supports several signed-in accounts).
pub fn gh_token(user: &str, host: &str) -> Result<String> {
    let mut cmd = Command::new("gh");
    cmd.args(["auth", "token", "--user", user, "--hostname", host]);
    for var in INHERITED_CREDENTIAL_VARS {
        cmd.env_remove(var);
    }
    let out = cmd
        .output()
        .context("running `gh auth token` (is the GitHub CLI installed?)")?;
    let token = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || token.is_empty() {
        bail!(
            "no GitHub CLI token for {user} on {host} ({}); sign that account in with: gh auth login --hostname {host}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_like_names() {
        for bad in ["", "..", "../x", "a/b", "a\\b", ".hidden", "a b"] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        validate_name("worker-1_a.b").unwrap();
    }

    #[test]
    fn config_round_trips_and_homes_are_per_profile() {
        let root = tempfile::tempdir().unwrap();
        let mut a = Profile::open_in(root.path(), "a").unwrap();
        assert!(!a.is_configured(Provider::Copilot));
        a.config.copilot = Some(CopilotAuth::Gh {
            user: "octo".into(),
            host: "github.com".into(),
        });
        a.config.codex = Some(CodexAuth::Login);
        a.save().unwrap();

        let json = std::fs::read_to_string(root.path().join("a/profile.json")).unwrap();
        assert!(json.contains(r#""source": "gh""#), "{json}");
        let a = Profile::open_in(root.path(), "a").unwrap();
        assert_eq!(
            a.config.copilot,
            Some(CopilotAuth::Gh {
                user: "octo".into(),
                host: "github.com".into()
            })
        );

        let b = Profile::open_in(root.path(), "b").unwrap();
        assert_ne!(
            a.home(Provider::Codex).unwrap(),
            b.home(Provider::Codex).unwrap()
        );
        assert!(root.path().join("a/codex").is_dir());
    }

    #[test]
    fn unconfigured_providers_fail_with_login_hint() {
        let root = tempfile::tempdir().unwrap();
        let p = Profile::open_in(root.path(), "w").unwrap();
        let err = p.copilot_credentials().unwrap_err().to_string();
        assert!(
            err.contains("p2pagents auth login copilot --profile w"),
            "{err}"
        );
        let err = p.bridge_env(&[Provider::Codex]).unwrap_err().to_string();
        assert!(err.contains("auth login codex --profile w"), "{err}");
    }

    #[test]
    fn bridge_env_points_each_provider_at_the_profile_home() {
        let root = tempfile::tempdir().unwrap();
        let mut p = Profile::open_in(root.path(), "w").unwrap();
        p.config.claude = Some(ClaudeAuth::Login);
        p.config.codex = Some(CodexAuth::Login);
        let env = p.bridge_env(&[Provider::Claude, Provider::Codex]).unwrap();
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| PathBuf::from(v))
        };
        assert_eq!(get("CLAUDE_CONFIG_DIR"), Some(root.path().join("w/claude")));
        assert_eq!(get("CODEX_HOME"), Some(root.path().join("w/codex")));
        assert!(get("ANTHROPIC_API_KEY").is_none());
    }
}
