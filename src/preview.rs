//! Previews of UX prototypes (see [`crate::spec`]): the `prototype/` files a plan
//! step created (from its patch) or a shared folder holds, served by the web UI at an
//! unguessable capability URL under a CSP sandbox, so a product owner can click
//! through the prototype before approving it. A prototype is untrusted content (a
//! model, or another peer, wrote it): the sandbox gives it an opaque origin with no
//! network, no form posts and no access to the UI's token or API.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Path (relative to `prototype/`) to content.
pub type Files = BTreeMap<String, Vec<u8>>;

pub const PROTOTYPE_DIR: &str = "prototype/";
/// Most bytes and files one preview holds.
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_FILES: usize = 200;
/// How long a preview stays available, and how many are kept.
const TTL: Duration = Duration::from_secs(2 * 60 * 60);
const MAX_PREVIEWS: usize = 50;

/// The response headers of every preview file: an opaque origin (no
/// `allow-same-origin`), inline code allowed but no network, no form posts.
pub const PREVIEW_CSP: &str = "sandbox allow-scripts allow-forms allow-modals; default-src 'self' 'unsafe-inline' data: blob:; connect-src 'none'; form-action 'none'; frame-ancestors 'self'; base-uri 'none'";

/// A safe relative path: plain segments only.
fn clean(path: &str) -> Option<String> {
    let ok = !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains('\0')
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != ".." && !seg.contains(':'));
    ok.then(|| path.to_owned())
}

fn add(files: &mut Files, total: &mut usize, path: String, content: Vec<u8>) -> bool {
    if files.len() >= MAX_FILES || *total + content.len() > MAX_BYTES {
        return false;
    }
    *total += content.len();
    files.insert(path, content);
    true
}

/// The files a patch (`git diff`) creates under `prototype/`. Files it only changes
/// are left out (their earlier content isn't in the patch), as are binary files.
pub fn from_patch(patch: &str) -> Files {
    struct Entry {
        path: Option<String>,
        new: bool,
        lines: Vec<String>,
        no_final_newline: bool,
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut in_hunk = false;
    for line in patch.split('\n') {
        if line.starts_with("diff --git ") {
            entries.push(Entry {
                path: None,
                new: false,
                lines: Vec::new(),
                no_final_newline: false,
            });
            in_hunk = false;
            continue;
        }
        let Some(entry) = entries.last_mut() else {
            continue;
        };
        if in_hunk {
            if let Some(added) = line.strip_prefix('+') {
                entry.lines.push(added.to_owned());
            } else if line.starts_with("\\ ") {
                entry.no_final_newline = true;
            } else if line.starts_with("@@") {
                // another hunk of the same file
            } else if !line.is_empty() {
                // context or removed lines: not a plain new file
                entry.new = false;
            }
        } else if line.starts_with("new file mode ") {
            entry.new = true;
        } else if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.trim_end_matches('\r');
            entry.path = path
                .strip_prefix("b/")
                .or(Some(path))
                .and_then(|p| p.strip_prefix(PROTOTYPE_DIR))
                .and_then(clean);
        } else if line.starts_with("@@") {
            in_hunk = true;
        }
    }
    let (mut files, mut total) = (Files::new(), 0);
    for e in entries {
        let Some(path) = e.path.filter(|_| e.new) else {
            continue;
        };
        let mut text = e.lines.join("\n");
        if !e.no_final_newline && !e.lines.is_empty() {
            text.push('\n');
        }
        if !add(&mut files, &mut total, path, text.into_bytes()) {
            break;
        }
    }
    files
}

/// The files in `dir/prototype/`, read now; symbolic links are not followed. Within
/// the limits, `index.html` and then the smallest files come first.
pub fn from_dir(dir: &Path) -> Files {
    let root = dir.join("prototype");
    let (mut files, mut total) = (Files::new(), 0);
    if std::fs::symlink_metadata(&root).map_or(true, |m| !m.is_dir()) {
        return files;
    }
    let mut candidates = Vec::new();
    let mut stack = vec![(root, String::new())];
    while let Some((folder, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten().take(MAX_FILES * 10) {
            let (Ok(kind), Some(name)) = (
                entry.file_type(),
                entry.file_name().to_str().map(str::to_owned),
            ) else {
                continue;
            };
            let Some(rel) = clean(&format!("{prefix}{name}")) else {
                continue;
            };
            if kind.is_dir() && stack.len() < MAX_FILES {
                stack.push((entry.path(), format!("{rel}/")));
            } else if kind.is_file()
                && let Ok(meta) = entry.metadata()
            {
                candidates.push((rel != "index.html", meta.len(), rel, entry.path()));
            }
        }
    }
    candidates.sort();
    for (_, size, rel, path) in candidates {
        if total + size as usize > MAX_BYTES || files.len() >= MAX_FILES {
            continue;
        }
        if let Ok(content) = std::fs::read(path) {
            add(&mut files, &mut total, rel, content);
        }
    }
    files
}

/// The prototype files of several patches (a later patch's version wins).
pub fn from_patches<'a>(patches: impl IntoIterator<Item = &'a str>) -> Files {
    let mut files = Files::new();
    for patch in patches {
        files.extend(from_patch(patch));
    }
    files
}

pub fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Previews by capability (a random id, the only thing needed to open one).
#[derive(Default)]
pub struct Previews {
    inner: Mutex<HashMap<String, (Instant, Arc<Files>)>>,
}

impl Previews {
    /// Keeps `files` for a while; returns the capability, or None if there is no
    /// `index.html` to open.
    pub fn add(&self, files: Files) -> Option<String> {
        if !files.contains_key("index.html") {
            return None;
        }
        let cap = data_encoding::HEXLOWER.encode(&iroh::SecretKey::generate().to_bytes()[..16]);
        let mut map = self.inner.lock().unwrap();
        map.retain(|_, (made, _)| made.elapsed() < TTL);
        while map.len() >= MAX_PREVIEWS {
            let oldest = map
                .iter()
                .min_by_key(|(_, (made, _))| *made)
                .map(|(k, _)| k.clone())
                .expect("not empty");
            map.remove(&oldest);
        }
        map.insert(cap.clone(), (Instant::now(), Arc::new(files)));
        Some(cap)
    }

    /// File `path` of preview `cap` (`index.html` for the folder itself).
    pub fn file(&self, cap: &str, path: &str) -> Option<(&'static str, Vec<u8>)> {
        let files = {
            let mut map = self.inner.lock().unwrap();
            map.retain(|_, (made, _)| made.elapsed() < TTL);
            map.get(cap)?.1.clone()
        };
        let path = path.trim_start_matches('/');
        let path = if path.is_empty() || path.ends_with('/') {
            format!("{path}index.html")
        } else {
            path.to_owned()
        };
        let content = files.get(&clean(&path)?)?.clone();
        Some((content_type(&path), content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "diff --git a/prototype/index.html b/prototype/index.html\n\
        new file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/prototype/index.html\n\
        @@ -0,0 +1,2 @@\n+<h1>Hi</h1>\n+<script src=\"app.js\"></script>\n\
        diff --git a/prototype/app.js b/prototype/app.js\nnew file mode 100644\n--- /dev/null\n+++ b/prototype/app.js\n\
        @@ -0,0 +1 @@\n+go()\n\\ No newline at end of file\n\
        diff --git a/src/main.rs b/src/main.rs\nnew file mode 100644\n--- /dev/null\n+++ b/src/main.rs\n@@ -0,0 +1 @@\n+fn main() {}\n\
        diff --git a/prototype/old.html b/prototype/old.html\nindex 1..2 100644\n--- a/prototype/old.html\n+++ b/prototype/old.html\n\
        @@ -1,2 +1,2 @@\n <p>\n-a\n+b\n\
        diff --git a/prototype/../evil b/prototype/../evil\nnew file mode 100644\n--- /dev/null\n+++ b/prototype/../evil\n@@ -0,0 +1 @@\n+x\n";

    #[test]
    fn previews_take_only_new_prototype_files_from_a_patch() {
        let files = from_patch(PATCH);
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            ["app.js", "index.html"],
            "changed files, other folders and unsafe paths are left out"
        );
        assert_eq!(
            files["index.html"],
            b"<h1>Hi</h1>\n<script src=\"app.js\"></script>\n"
        );
        assert_eq!(files["app.js"], b"go()");
    }

    #[test]
    fn previews_are_served_by_capability_only_and_expire() {
        let previews = Previews::default();
        assert!(
            previews
                .add(Files::from([("a.html".into(), b"x".to_vec())]))
                .is_none()
        );
        let cap = previews.add(from_patch(PATCH)).unwrap();
        assert_eq!(cap.len(), 32);
        let (kind, body) = previews.file(&cap, "").unwrap();
        assert_eq!(kind, "text/html; charset=utf-8");
        assert!(body.starts_with(b"<h1>Hi"));
        assert_eq!(
            previews.file(&cap, "app.js").unwrap().0,
            "text/javascript; charset=utf-8"
        );
        assert!(previews.file(&cap, "../app.js").is_none());
        assert!(previews.file(&cap, "missing.css").is_none());
        assert!(previews.file("0000", "").is_none());
        previews.inner.lock().unwrap().get_mut(&cap).unwrap().0 =
            Instant::now() - TTL - Duration::from_secs(1);
        assert!(previews.file(&cap, "").is_none(), "expired");
    }

    #[test]
    fn folder_previews_are_capped_and_do_not_follow_links() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("prototype/css")).unwrap();
        std::fs::write(dir.path().join("prototype/index.html"), "<p>ok</p>").unwrap();
        std::fs::write(dir.path().join("prototype/css/site.css"), "p{}").unwrap();
        std::fs::write(dir.path().join("prototype/big.bin"), vec![0u8; MAX_BYTES]).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("prototype/s.txt"),
        )
        .is_ok();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(
            outside.path().join("secret.txt"),
            dir.path().join("prototype/s.txt"),
        )
        .is_ok();
        let files = from_dir(dir.path());
        assert!(files.contains_key("index.html") && files.contains_key("css/site.css"));
        assert!(
            !files.contains_key("big.bin"),
            "it doesn't fit with the others"
        );
        assert!(
            !files.contains_key("big.bin"),
            "it doesn't fit with the others"
        );
        assert!(files.values().map(Vec::len).sum::<usize>() <= MAX_BYTES);
        if linked {
            assert!(!files.contains_key("s.txt"), "links are not followed");
        }
    }
}
