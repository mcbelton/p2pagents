//! GitHub: a profile signs in with an account of the GitHub CLI, links repositories
//! (cloned with that account into `~/.proofkin-repos/<profile>/<name>` and shared with
//! peers as a folder with a git worktree per task), keeps them in sync, pushes the
//! branches agents' tasks committed to (`p2p/...`) and opens pull requests.
//!
//! The token never appears on a command line or in the clone's `.git/config`: git
//! gets it as an HTTP header through `GIT_CONFIG_*` environment variables (like
//! `actions/checkout`), and the GitHub CLI through `GH_TOKEN`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::profile::{DEFAULT_GITHUB_HOST, INHERITED_CREDENTIAL_VARS, validate_name};

#[derive(Clone)]
pub struct GithubCredentials {
    pub user: String,
    pub host: String,
    pub token: String,
}

impl std::fmt::Debug for GithubCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubCredentials")
            .field("user", &self.user)
            .field("host", &self.host)
            .field("token", &"<redacted>")
            .finish()
    }
}

/// A repository linked to a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    /// Folder name peers use (the share name).
    pub name: String,
    /// `owner/repo`.
    pub slug: String,
    pub host: String,
    pub path: PathBuf,
    /// The branch the clone tracks (the repository's default branch).
    pub branch: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoStore {
    #[serde(default)]
    pub repos: Vec<Repo>,
}

impl RepoStore {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Result<&Repo> {
        self.repos
            .iter()
            .find(|r| r.name == name)
            .with_context(|| format!("there is no linked repository '{name}' (see repo list)"))
    }
}

/// `(host, owner/repo)` from `owner/repo`, `host/owner/repo` or an https URL.
pub fn parse_repo(input: &str, default_host: &str) -> Result<(String, String)> {
    let trimmed = input
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let parts: Vec<&str> = trimmed.split('/').filter(|p| !p.is_empty()).collect();
    let (host, owner, repo) = match parts.as_slice() {
        [owner, repo] => (default_host, *owner, *repo),
        [host, owner, repo] if host.contains('.') => (*host, *owner, *repo),
        _ => bail!("expected owner/repo or https://{default_host}/owner/repo, got {input:?}"),
    };
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !s.starts_with('.')
    };
    if !ok(owner) || !ok(repo) || !ok(host) {
        bail!("{input:?} is not a valid repository");
    }
    Ok((host.to_owned(), format!("{owner}/{repo}")))
}

/// GitHub CLI accounts signed in on this machine: `(login, host, active)`.
pub fn gh_accounts() -> Vec<(String, String, bool)> {
    let out = std::process::Command::new(gh_program())
        .args(["auth", "status", "--json", "hosts"])
        .stdin(Stdio::null())
        .output();
    let Ok(out) = out else { return Vec::new() };
    let hosts: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let mut accounts = Vec::new();
    if let Some(hosts) = hosts.get("hosts").and_then(|h| h.as_object()) {
        for (host, list) in hosts {
            for a in list.as_array().into_iter().flatten() {
                if a.get("state").and_then(|s| s.as_str()) == Some("success")
                    && let Some(login) = a.get("login").and_then(|l| l.as_str())
                {
                    let active = a.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                    accounts.push((login.to_owned(), host.clone(), active));
                }
            }
        }
    }
    accounts
}

/// The GitHub CLI (`PROOFKIN_GH` overrides it, e.g. for tests).
fn gh_program() -> PathBuf {
    crate::profile::env_setting_os("GH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("gh"))
}

impl GithubCredentials {
    /// git configuration (through the environment) that authenticates to the host.
    pub(crate) fn git_env(&self) -> Vec<(String, String)> {
        let basic =
            data_encoding::BASE64.encode(format!("x-access-token:{}", self.token).as_bytes());
        vec![
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_CONFIG_COUNT".into(), "1".into()),
            (
                "GIT_CONFIG_KEY_0".into(),
                format!("http.https://{}/.extraheader", self.host),
            ),
            (
                "GIT_CONFIG_VALUE_0".into(),
                format!("AUTHORIZATION: basic {basic}"),
            ),
        ]
    }

    async fn git(&self, dir: Option<&Path>, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("git");
        if let Some(dir) = dir {
            cmd.arg("-C").arg(dir);
        }
        cmd.args(args).stdin(Stdio::null()).kill_on_drop(true);
        for var in INHERITED_CREDENTIAL_VARS {
            cmd.env_remove(var);
        }
        cmd.envs(self.git_env());
        let out = cmd.output().await.context("running git")?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args.first().copied().unwrap_or_default(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// The https URL of `slug` on this account's host.
    pub fn url(&self, slug: &str) -> String {
        format!("https://{}/{slug}.git", self.host)
    }

    /// Clones `url` into `dir` (which must not exist); returns the checked-out branch.
    pub async fn clone_into(&self, url: &str, dir: &Path) -> Result<String> {
        if dir.exists() {
            bail!("{} already exists", dir.display());
        }
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let target = dir.to_string_lossy().into_owned();
        self.git(None, &["clone", "-q", url, &target])
            .await
            .with_context(|| format!("cloning {url} as {}", self.user))?;
        self.git(Some(dir), &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
    }

    /// Fetches and fast-forwards the clone's branch; returns the new head commit.
    pub async fn sync(&self, repo: &Repo) -> Result<String> {
        self.git(Some(&repo.path), &["fetch", "-q", "--prune", "origin"])
            .await?;
        let upstream = format!("origin/{}", repo.branch);
        self.git(Some(&repo.path), &["merge", "-q", "--ff-only", &upstream])
            .await
            .context("the clone has local changes on its branch; resolve them in the folder")?;
        self.git(Some(&repo.path), &["rev-parse", "--short", "HEAD"])
            .await
    }

    /// Pushes local branch `branch` to the repository.
    pub async fn push(&self, repo: &Repo, branch: &str) -> Result<()> {
        if !branch.starts_with("p2p/") {
            bail!("only branches agents' tasks made (p2p/...) are pushed from here");
        }
        let refspec = format!("refs/heads/{branch}:refs/heads/{branch}");
        self.git(Some(&repo.path), &["push", "-q", "origin", &refspec])
            .await
            .map(drop)
    }

    /// Opens a pull request from `head` into the repository's branch; returns its URL.
    pub async fn create_pr(
        &self,
        repo: &Repo,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let mut cmd = Command::new(gh_program());
        cmd.args([
            "pr",
            "create",
            "--repo",
            &format!("{}/{}", repo.host, repo.slug),
            "--base",
            &repo.branch,
            "--head",
            head,
            "--title",
            title,
            "--body",
            body,
        ])
        .stdin(Stdio::null())
        .kill_on_drop(true);
        for var in INHERITED_CREDENTIAL_VARS {
            cmd.env_remove(var);
        }
        cmd.env("GH_TOKEN", &self.token)
            .env("GH_HOST", &self.host)
            .env("GH_PROMPT_DISABLED", "1");
        let out = cmd
            .output()
            .await
            .context("running the GitHub CLI (gh) to open the pull request")?;
        if !out.status.success() {
            bail!(
                "opening the pull request failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text
            .lines()
            .rev()
            .find(|l| l.starts_with("https://"))
            .unwrap_or(text.trim())
            .to_owned())
    }
}

/// A branch an agent's task committed to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskBranch {
    pub name: String,
    pub unix: u64,
    pub subject: String,
}

/// The clone's `p2p/...` branches, newest first.
pub async fn task_branches(repo: &Repo) -> Result<Vec<TaskBranch>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(&repo.path)
        .args([
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)%09%(committerdate:unix)%09%(subject)",
            "refs/heads/p2p/",
        ])
        .stdin(Stdio::null())
        .output()
        .await
        .context("running git")?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\t');
            Some(TaskBranch {
                name: parts.next()?.to_owned(),
                unix: parts.next()?.parse().ok()?,
                subject: parts.next().unwrap_or_default().to_owned(),
            })
        })
        .collect())
}

/// A folder name for `slug` (`owner/web-app` -> `web-app`).
pub fn default_name(slug: &str) -> Result<String> {
    let name = slug.rsplit('/').next().unwrap_or(slug).replace('.', "-");
    validate_name(&name).with_context(|| format!("pick a name with --as for {slug}"))?;
    Ok(name)
}

/// Links `input` (`owner/repo` or a URL) to `profile`: clones it with the profile's
/// GitHub account and shares it as folder `name` (default: the repository name)
/// with a worktree per task. Returns the repository and the profile's new shares.
pub async fn link(
    profile: &crate::Profile,
    input: &str,
    name: Option<&str>,
    access: crate::Access,
    checks: std::collections::BTreeMap<String, String>,
) -> Result<(Repo, crate::shares::ShareStore)> {
    let creds = profile.github()?;
    let (host, slug) = parse_repo(input, &creds.host)?;
    if host != creds.host {
        bail!(
            "{slug} is on {host}, but profile '{}' is signed in to {}",
            profile.name,
            creds.host
        );
    }
    let url = creds.url(&slug);
    link_url(profile, &creds, &url, &host, &slug, name, access, checks).await
}

/// [`link`] from an explicit clone URL.
#[allow(clippy::too_many_arguments)]
pub async fn link_url(
    profile: &crate::Profile,
    creds: &GithubCredentials,
    url: &str,
    host: &str,
    slug: &str,
    name: Option<&str>,
    access: crate::Access,
    checks: std::collections::BTreeMap<String, String>,
) -> Result<(Repo, crate::shares::ShareStore)> {
    let name = match name {
        Some(n) => {
            validate_name(n).context("invalid folder name")?;
            n.to_owned()
        }
        None => default_name(slug)?,
    };
    let mut repos = RepoStore::load(&profile.repos_path())?;
    if repos.repos.iter().any(|r| r.name == name) {
        bail!("a repository named '{name}' is already linked");
    }
    let mut shares = profile.shares()?;
    if shares.get(&name).is_some() {
        bail!(
            "profile '{}' already shares a folder named '{name}'; pick another name",
            profile.name
        );
    }
    let path = profile.repos_dir().join(&name);
    let branch = creds.clone_into(url, &path).await?;
    shares.add_with(&name, &path, access, checks, true)?;
    shares.save(&profile.shares_path())?;
    let repo = Repo {
        name,
        slug: slug.to_owned(),
        host: host.to_owned(),
        path: shares.shares.last().map(|s| s.path.clone()).unwrap_or(path),
        branch,
    };
    repos.repos.push(repo.clone());
    repos.save(&profile.repos_path())?;
    Ok((repo, shares))
}

/// Unlinks repository `name`: stops sharing it and forgets it; with `delete`, also
/// removes the clone. Returns the profile's new shares.
pub fn unlink(
    profile: &crate::Profile,
    name: &str,
    delete: bool,
) -> Result<crate::shares::ShareStore> {
    let mut repos = RepoStore::load(&profile.repos_path())?;
    let repo = repos.get(name)?.clone();
    repos.repos.retain(|r| r.name != name);
    repos.save(&profile.repos_path())?;
    let mut shares = profile.shares()?;
    shares.remove(name)?;
    shares.save(&profile.shares_path())?;
    if delete && repo.path.starts_with(profile.repos_dir()) {
        std::fs::remove_dir_all(&repo.path)
            .with_context(|| format!("removing {}", repo.path.display()))?;
    }
    Ok(shares)
}

/// The default host for repositories (github.com).
pub fn default_host() -> &'static str {
    DEFAULT_GITHUB_HOST
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names_are_parsed_strictly() {
        let gh = "github.com";
        assert_eq!(
            parse_repo("mcbelton/proofkin", gh).unwrap(),
            (gh.into(), "mcbelton/proofkin".into())
        );
        assert_eq!(
            parse_repo("https://github.com/mcbelton/proofkin.git", gh).unwrap(),
            (gh.into(), "mcbelton/proofkin".into())
        );
        assert_eq!(
            parse_repo("corp.ghe.com/team/app", gh).unwrap(),
            ("corp.ghe.com".into(), "team/app".into())
        );
        for bad in [
            "proofkin",
            "a/b/c/d",
            "owner/../x",
            "owner/re po",
            "https://github.com/o/r?x=1",
            "-x/../y",
        ] {
            assert!(parse_repo(bad, gh).is_err(), "{bad}");
        }
        assert_eq!(default_name("owner/web.app").unwrap(), "web-app");
        let creds = GithubCredentials {
            user: "u".into(),
            host: "github.com".into(),
            token: "s3cret".into(),
        };
        assert!(!format!("{creds:?}").contains("s3cret"));
    }
}
