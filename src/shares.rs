//! Folders an agent shares with its peers. Peer tasks run only inside a shared
//! folder the owner granted to that peer; the agent's own working folder is never
//! exposed. Every profile has a built-in read-only `data` share, its own
//! `.p2pdata/<profile>` folder, for information the owner wants peers to see.
//! Other folders (e.g. a repository for coding tasks) are shared explicitly with
//! `p2pagents share add`, each with the most access any peer's task may get there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::llm::Access;
use crate::profile::validate_name;

/// Name of the built-in share (the profile's `.p2pdata` folder).
pub const DATA_SHARE: &str = "data";

/// A folder shared with peers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Share {
    pub name: String,
    pub path: PathBuf,
    /// Most a peer's task may do here, whatever the peer was granted.
    pub max_access: Access,
    /// Owner-defined commands (name -> shell command) a task may ask the runtime to
    /// run after the model, as evidence (e.g. `test` -> `cargo test`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub checks: BTreeMap<String, String>,
    /// Run each editing task in its own git worktree and branch, and commit its work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worktrees: bool,
}

/// What a peer learns about a share: its name, the access it would get and the
/// names of its checks. Local paths and commands are never sent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareInfo {
    pub name: String,
    pub access: Access,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worktrees: bool,
}

/// Parses `name=command` check definitions.
pub fn parse_checks(defs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut checks = BTreeMap::new();
    for def in defs {
        let (name, command) = def
            .split_once('=')
            .with_context(|| format!("check {def:?} must look like name=command"))?;
        let (name, command) = (name.trim(), command.trim());
        validate_name(name).with_context(|| format!("invalid check name {name:?}"))?;
        if command.is_empty() {
            bail!("check '{name}' has no command");
        }
        checks.insert(name.to_owned(), command.to_owned());
    }
    Ok(checks)
}

/// The access a peer's task gets in a share.
pub fn effective_access(peer_grant: Access, share_max: Access) -> Access {
    peer_grant.min(share_max)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareStore {
    #[serde(default)]
    pub shares: Vec<Share>,
}

impl ShareStore {
    /// Only the built-in read-only `data` share at `data`.
    pub fn with_data(data: PathBuf) -> Self {
        Self {
            shares: vec![Share {
                name: DATA_SHARE.into(),
                path: data,
                max_access: Access::ReadOnly,
                checks: BTreeMap::new(),
                worktrees: false,
            }],
        }
    }

    /// The owner's shares from `path` plus the built-in `data` share at `data`.
    pub fn load(path: &Path, data: PathBuf) -> Result<Self> {
        let mut store = Self::with_data(data);
        if path.is_file() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            let saved: ShareStore = serde_json::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            store
                .shares
                .extend(saved.shares.into_iter().filter(|s| s.name != DATA_SHARE));
        }
        Ok(store)
    }

    /// Saves the owner's shares (the built-in `data` share is implied).
    pub fn save(&self, path: &Path) -> Result<()> {
        let owned = ShareStore {
            shares: self
                .shares
                .iter()
                .filter(|s| s.name != DATA_SHARE)
                .cloned()
                .collect(),
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(&owned)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Shares `path` (an existing folder) as `name`, replacing a share of that name.
    pub fn add(&mut self, name: &str, path: &Path, max_access: Access) -> Result<&Share> {
        self.add_with(name, path, max_access, BTreeMap::new(), false)
    }

    /// [`ShareStore::add`] with checks and per-task git worktrees (needs a git repository).
    pub fn add_with(
        &mut self,
        name: &str,
        path: &Path,
        max_access: Access,
        checks: BTreeMap<String, String>,
        worktrees: bool,
    ) -> Result<&Share> {
        validate_name(name).context("invalid share name")?;
        if name == DATA_SHARE {
            bail!("'{DATA_SHARE}' is the built-in share (the profile's .p2pdata folder)");
        }
        let path = std::fs::canonicalize(path)
            .with_context(|| format!("{} is not an existing folder", path.display()))?;
        if !path.is_dir() {
            bail!("{} is not a folder", path.display());
        }
        let path = plain_path(path);
        if worktrees && !path.join(".git").exists() {
            bail!(
                "--worktrees needs a git repository; {} has no .git",
                path.display()
            );
        }
        self.shares.retain(|s| s.name != name);
        self.shares.push(Share {
            name: name.to_owned(),
            path,
            max_access,
            checks,
            worktrees,
        });
        Ok(self.shares.last().expect("just pushed"))
    }

    pub fn remove(&mut self, name: &str) -> Result<bool> {
        if name == DATA_SHARE {
            bail!("the built-in '{DATA_SHARE}' share can't be removed");
        }
        let before = self.shares.len();
        self.shares.retain(|s| s.name != name);
        Ok(self.shares.len() != before)
    }

    pub fn get(&self, name: &str) -> Option<&Share> {
        self.shares.iter().find(|s| s.name == name)
    }

    /// Shares among `granted` names, with the access a peer granted `grant` gets.
    pub fn visible(&self, granted: &[String], grant: Access) -> Vec<ShareInfo> {
        self.shares
            .iter()
            .filter(|s| granted.contains(&s.name))
            .map(|s| ShareInfo {
                name: s.name.clone(),
                access: effective_access(grant, s.max_access),
                checks: s.checks.keys().cloned().collect(),
                worktrees: s.worktrees,
            })
            .collect()
    }
}

/// `C:\x` instead of Windows' verbatim `\\?\C:\x` from `canonicalize`, for display.
fn plain_path(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_folder_is_built_in_read_only_and_per_profile() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("profiles");
        let worker = crate::Profile::open_in(&root, "worker").unwrap();
        let other = crate::Profile::open_in(&root, "other").unwrap();
        assert_ne!(worker.data_folder(), other.data_folder());
        assert!(
            worker
                .data_folder()
                .ends_with(Path::new(".p2pdata").join("worker"))
        );
        assert!(!worker.data_folder().starts_with(&worker.dir));
        // Providers deny tasks the profiles folder, so task worktrees live outside it.
        assert!(!worker.worktrees_dir().starts_with(&root));
        assert_ne!(worker.worktrees_dir(), other.worktrees_dir());

        let store = ShareStore::load(&dir.path().join("none.json"), worker.data_folder()).unwrap();
        assert_eq!(store.shares.len(), 1);
        assert_eq!(store.get(DATA_SHARE).unwrap().max_access, Access::ReadOnly);
        let mut store = store;
        assert!(store.remove(DATA_SHARE).is_err());
        assert!(store.add(DATA_SHARE, dir.path(), Access::Full).is_err());
    }

    #[test]
    fn shares_persist_and_names_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let file = dir.path().join("shares.json");
        let mut store = ShareStore::with_data(dir.path().join("data"));
        store.add("repo", &repo, Access::WorkspaceWrite).unwrap();
        assert!(store.add("../x", &repo, Access::ReadOnly).is_err());
        assert!(
            store
                .add("missing", &dir.path().join("nope"), Access::ReadOnly)
                .is_err()
        );
        store.save(&file).unwrap();
        assert!(!std::fs::read_to_string(&file).unwrap().contains("\"data\""));

        let loaded = ShareStore::load(&file, dir.path().join("data")).unwrap();
        let names: Vec<&str> = loaded.shares.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, [DATA_SHARE, "repo"]);
        assert_eq!(
            loaded.get("repo").unwrap().path,
            plain_path(std::fs::canonicalize(&repo).unwrap())
        );
        assert!(
            !loaded
                .get("repo")
                .unwrap()
                .path
                .to_string_lossy()
                .starts_with(r"\\?\")
        );
        let mut loaded = loaded;
        assert!(loaded.remove("repo").unwrap());
        assert!(!loaded.remove("repo").unwrap());
    }

    #[test]
    fn effective_access_is_the_lower_of_grant_and_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = ShareStore::with_data(dir.path().to_path_buf());
        store
            .add("repo", dir.path(), Access::WorkspaceWrite)
            .unwrap();
        assert_eq!(
            effective_access(Access::Full, Access::ReadOnly),
            Access::ReadOnly
        );
        assert_eq!(
            effective_access(Access::ReadOnly, Access::Full),
            Access::ReadOnly
        );

        let granted = vec![DATA_SHARE.to_owned(), "repo".to_owned(), "gone".to_owned()];
        let seen = store.visible(&granted, Access::Full);
        assert_eq!(
            seen,
            [
                ShareInfo {
                    name: DATA_SHARE.into(),
                    access: Access::ReadOnly,
                    ..Default::default()
                },
                ShareInfo {
                    name: "repo".into(),
                    access: Access::WorkspaceWrite,
                    ..Default::default()
                },
            ]
        );
        assert!(
            store.visible(&["repo".to_owned()], Access::ReadOnly)[0].access == Access::ReadOnly
        );
        assert!(store.visible(&[], Access::Full).is_empty());
    }
}
