//! Path rules every provider applies to a task's file access: stay inside the
//! workspace, never touch credential files, and never write files that make tools
//! run code later (git hooks/config, editor tasks, agent settings).

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// Directory names whose contents are credentials or agent state.
const SENSITIVE_DIRS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".proofkin",
    ".p2pagents",
    ".copilot",
];

/// File names that usually hold secrets.
const SENSITIVE_FILES: &[&str] = &[
    ".env",
    ".envrc",
    ".npmrc",
    ".yarnrc.yml",
    ".pypirc",
    ".netrc",
    "_netrc",
    ".pgpass",
    ".git-credentials",
    ".htpasswd",
    "credentials",
    "credentials.json",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
    "secrets.toml",
];

/// Private key and keystore extensions.
const SENSITIVE_EXTENSIONS: &[&str] = &[
    "pem", "key", "p12", "pfx", "jks", "keystore", "kdbx", "ppk", "gpg", "asc",
];

/// Files that, if written, run code or change agent permissions later.
const PROTECTED_DIRS: &[&str] = &[
    ".git", ".vscode", ".idea", ".claude", ".codex", ".copilot", ".cursor",
];
const PROTECTED_FILES: &[&str] = &[".mcp.json", ".gitmodules"];

/// Credential locations in the home folder, relative to it.
const HOME_SECRETS: &[&str] = &[
    ".ssh",
    ".aws",
    ".azure",
    ".gnupg",
    ".kube",
    ".docker",
    ".proofkin",
    ".p2pagents",
    ".copilot",
    ".config/gh",
    ".git-credentials",
    ".netrc",
    ".npmrc",
    ".pypirc",
];

/// Existing credential locations in the home folder (and the profiles folder) that
/// OS sandboxes should deny to sandboxed commands.
pub fn home_secret_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::home_dir()
        .map(|home| HOME_SECRETS.iter().map(|rel| home.join(rel)).collect())
        .unwrap_or_default();
    if let Ok(root) = crate::profile::profiles_root() {
        paths.push(root);
    }
    paths.retain(|p| p.exists());
    paths.sort();
    paths.dedup();
    paths
}

/// A task's workspace, canonicalized once so symlinks and `..` can't escape it.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(dir: &Path) -> std::io::Result<Self> {
        Ok(Self {
            root: std::fs::canonicalize(dir)?,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Why reading `path` is refused, or `None` if allowed.
    pub fn check_read(&self, path: &str) -> Option<String> {
        let rel = match self.relative(path) {
            Ok(rel) => rel,
            Err(reason) => return Some(reason),
        };
        is_sensitive(&rel).then(|| format!("{path} may contain credentials"))
    }

    /// Why writing `path` is refused, or `None` if allowed.
    pub fn check_write(&self, path: &str) -> Option<String> {
        let rel = match self.relative(path) {
            Ok(rel) => rel,
            Err(reason) => return Some(reason),
        };
        if is_sensitive(&rel) {
            Some(format!("{path} may contain credentials"))
        } else if is_protected(&rel) {
            Some(format!(
                "{path} is tool configuration that could run code; edit it yourself"
            ))
        } else {
            None
        }
    }

    /// Why the shell command `text` is refused, or `None`. Best effort only: it
    /// catches plain URLs, paths outside the workspace and credential files named
    /// in the command, not obfuscated ones, so it is no sandbox.
    pub fn check_command(&self, text: &str) -> Option<String> {
        let lower = text.to_lowercase();
        if lower.contains("://") {
            return Some("shell commands may not access URLs".into());
        }
        const HOME_REFS: &[&str] = &[
            "$home",
            "${home}",
            "$env:userprofile",
            "$env:home",
            "$env:appdata",
            "$env:localappdata",
            "%userprofile%",
            "%appdata%",
            "%localappdata%",
            "~/",
            "~\\",
        ];
        if HOME_REFS.iter().any(|r| lower.contains(r)) {
            return Some("shell commands may not reference the home folder".into());
        }
        let separators = |c: char| c.is_whitespace() || "'\"`;|&()<>,={}".contains(c);
        text.split(separators)
            .filter(|t| !t.is_empty() && !t.starts_with('-'))
            .filter(|t| !(cfg!(windows) && t.starts_with('/')))
            .filter(|t| !["/dev/null", "/dev/stdout", "/dev/stderr", "nul"].contains(t))
            .find_map(|t| {
                if t == "~" {
                    Some("shell commands may not reference the home folder".into())
                } else {
                    self.check_read(t)
                }
            })
    }

    /// `path` relative to the workspace, after resolving `..` and symlinks.
    fn relative(&self, path: &str) -> Result<PathBuf, String> {
        let root = self.root.to_string_lossy();
        let root = root.strip_prefix(r"\\?\").unwrap_or(&root);
        let outside = || format!("{path} is outside the workspace {root}");
        let resolved = resolve(&self.root, Path::new(path)).ok_or_else(outside)?;
        resolved
            .strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .map_err(|_| outside())
    }
}

/// Absolute, `..`-free path with symlinks resolved on the part that exists.
fn resolve(root: &Path, path: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let mut normal = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                if !normal.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    let mut existing = normal;
    let mut missing = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&existing) {
            return Some(missing.into_iter().rev().fold(real, |p, name| p.join(name)));
        }
        missing.push(existing.file_name()?.to_owned());
        if !existing.pop() {
            return None;
        }
    }
}

fn lower(name: &OsStr) -> String {
    name.to_string_lossy().to_lowercase()
}

fn is_sensitive(rel: &Path) -> bool {
    let names: Vec<String> = rel.components().map(|c| lower(c.as_os_str())).collect();
    if names
        .iter()
        .any(|n| SENSITIVE_DIRS.contains(&n.as_str()) || n.starts_with("id_rsa"))
    {
        return true;
    }
    let Some(file) = names.last() else {
        return false;
    };
    let template = [".example", ".sample", ".template", ".dist"]
        .iter()
        .any(|suffix| file.ends_with(suffix));
    if SENSITIVE_FILES.contains(&file.as_str())
        || (file.starts_with(".env.") && !template)
        || ["id_dsa", "id_ecdsa", "id_ed25519"]
            .iter()
            .any(|key| file.starts_with(key))
    {
        return true;
    }
    Path::new(file)
        .extension()
        .is_some_and(|ext| SENSITIVE_EXTENSIONS.contains(&lower(ext).as_str()))
}

fn is_protected(rel: &Path) -> bool {
    let names: Vec<String> = rel.components().map(|c| lower(c.as_os_str())).collect();
    names.iter().any(|n| PROTECTED_DIRS.contains(&n.as_str()))
        || names
            .last()
            .is_some_and(|f| PROTECTED_FILES.contains(&f.as_str()))
        || names.starts_with(&[".github".to_owned(), "hooks".to_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, tempfile::TempDir, Workspace) {
        let ws = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(ws.path().join("src")).unwrap();
        std::fs::write(ws.path().join("src/main.rs"), "").unwrap();
        std::fs::write(outside.path().join("secret.txt"), "").unwrap();
        let workspace = Workspace::new(ws.path()).unwrap();
        (ws, outside, workspace)
    }

    #[test]
    fn reads_stay_inside_the_workspace() {
        let (ws, outside, w) = setup();
        let inside = ws.path().join("src/main.rs");
        assert_eq!(w.check_read(inside.to_str().unwrap()), None);
        assert_eq!(w.check_read("src/main.rs"), None);
        assert_eq!(w.check_read("."), None);
        assert_eq!(w.check_read("src/new_file.rs"), None);
        let secret = outside.path().join("secret.txt");
        assert!(w.check_read(secret.to_str().unwrap()).is_some());
        assert!(w.check_read("../secret.txt").is_some());
        assert!(w.check_read("src/../../x").is_some());
        assert!(w.check_read("").is_some());
        assert!(w.check_read(outside.path().to_str().unwrap()).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape() {
        let (ws, outside, w) = setup();
        std::os::unix::fs::symlink(outside.path(), ws.path().join("link")).unwrap();
        assert!(w.check_read("link/secret.txt").is_some());
    }

    #[test]
    fn credentials_are_never_readable() {
        let (_ws, _outside, w) = setup();
        for path in [
            ".env",
            ".env.local",
            "config/.ENV.production",
            "deploy/server.pem",
            "certs/site.key",
            ".ssh/config",
            "home/.aws/credentials",
            "id_rsa",
            "keys/id_ed25519.pub",
            ".npmrc",
            ".git-credentials",
            "secrets.json",
        ] {
            assert!(w.check_read(path).is_some(), "{path} should be refused");
        }
        for path in [
            ".env.example",
            "src/keyboard.rs",
            "docs/keys.md",
            "README.md",
        ] {
            assert_eq!(w.check_read(path), None, "{path} should be readable");
        }
    }

    #[test]
    fn shell_commands_are_scanned_for_obvious_escapes() {
        let (_ws, outside, w) = setup();
        for ok in [
            "cargo test --all",
            "git status",
            "npm run build 2>&1 > /dev/null",
            "Get-ChildItem src -Recurse",
        ] {
            assert_eq!(w.check_command(ok), None, "{ok} should run");
        }
        let secret = outside.path().join("secret.txt");
        for bad in [
            "curl https://example.com".to_owned(),
            "Invoke-WebRequest -Uri http://10.0.0.1/x".to_owned(),
            format!("Get-Content {}", secret.display()),
            format!("type \"{}\"", secret.display()),
            "cat ../../etc/passwd".to_owned(),
            "cat ~/.ssh/id_rsa".to_owned(),
            "Get-Content $env:USERPROFILE\\.gitconfig".to_owned(),
            "type %APPDATA%\\x".to_owned(),
            "cat .env".to_owned(),
            "cd ~".to_owned(),
        ] {
            assert!(w.check_command(&bad).is_some(), "{bad} should be refused");
        }
    }

    #[test]
    fn writes_cannot_plant_code_that_runs_later() {
        let (_ws, _outside, w) = setup();
        for path in [
            ".git/hooks/pre-commit",
            ".git/config",
            ".vscode/tasks.json",
            ".github/hooks/hook.json",
            ".claude/settings.json",
            ".mcp.json",
            ".env",
            ".envrc",
        ] {
            assert!(w.check_write(path).is_some(), "{path} should be refused");
        }
        assert_eq!(w.check_read(".git/config"), None);
        for path in ["src/lib.rs", ".github/workflows/ci.yml", ".gitignore"] {
            assert_eq!(w.check_write(path), None, "{path} should be writable");
        }
    }
}
