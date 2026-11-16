//! Previews of what a feature's reviewers look at: the UX prototype (see
//! [`crate::spec`]), the review decks and the design documents (see
//! [`crate::design`]). They come from the files a plan step created (its patch), a
//! shared folder, or an approval request. The web UI serves them at an unguessable
//! capability URL under a CSP sandbox, so a reviewer can click through them before
//! answering. A model or another peer wrote them, so they are untrusted: the sandbox
//! gives them an opaque origin with no network, no form posts and no access to the
//! UI's token or API, and decks and documents are rendered without their raw HTML
//! (see [`crate::render`]).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::render;

/// Previewable files by path in the project (`prototype/...`, `design/....md`,
/// `review/....md`).
pub type Sources = BTreeMap<String, Vec<u8>>;
/// A preview site: path (relative to its root) to content.
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

/// Whether `path` (in the project) is shown in previews.
pub fn previewable(path: &str) -> bool {
    clean(path).is_some()
        && (path.starts_with(PROTOTYPE_DIR)
            || ((path.starts_with("design/") || path.starts_with("review/"))
                && path.ends_with(".md")))
}

fn add(files: &mut Sources, total: &mut usize, path: String, content: Vec<u8>) -> bool {
    if files.len() >= MAX_FILES || *total + content.len() > MAX_BYTES {
        return false;
    }
    *total += content.len();
    files.insert(path, content);
    true
}

/// The previewable files a patch (`git diff`) creates. Files it only changes are
/// left out (their earlier content isn't in the patch), as are binary files.
pub fn sources_from_patch(patch: &str) -> Sources {
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
            let path = path.strip_prefix("b/").unwrap_or(path);
            entry.path = previewable(path).then(|| path.to_owned());
        } else if line.starts_with("@@") {
            in_hunk = true;
        }
    }
    let (mut files, mut total) = (Sources::new(), 0);
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

/// The previewable files of several patches (a later patch's version wins).
pub fn sources_from_patches<'a>(patches: impl IntoIterator<Item = &'a str>) -> Sources {
    let mut files = Sources::new();
    for patch in patches {
        files.extend(sources_from_patch(patch));
    }
    files
}

/// The previewable files in `dir`, read now; symbolic links are not followed. Within
/// the limits, documents and `prototype/index.html` come first, then the smallest
/// files.
pub fn sources_from_dir(dir: &Path) -> Sources {
    let mut candidates = Vec::new();
    for top in ["prototype", "design", "review"] {
        let root = dir.join(top);
        if std::fs::symlink_metadata(&root).map_or(true, |m| !m.is_dir()) {
            continue;
        }
        let mut stack = vec![(root, format!("{top}/"))];
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
                    && previewable(&rel)
                    && let Ok(meta) = entry.metadata()
                {
                    let first = is_doc(&rel) || rel == "prototype/index.html";
                    candidates.push((!first, meta.len(), rel, entry.path()));
                }
            }
        }
    }
    candidates.sort();
    let (mut files, mut total) = (Sources::new(), 0);
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

fn is_doc(path: &str) -> bool {
    path.ends_with(".md") && !path.starts_with(PROTOTYPE_DIR)
}

/// What a preview of `sources` shows: "review pack" (decks or design documents, with
/// any prototype), "prototype", or None if there is nothing to open.
pub fn label(sources: &Sources) -> Option<&'static str> {
    if sources.keys().any(|p| is_doc(p)) {
        Some("review pack")
    } else if sources.contains_key("prototype/index.html") {
        Some("prototype")
    } else {
        None
    }
}

/// The review documents in the order reviewers take them, with their labels.
const REVIEWS: &[(&str, &str)] = &[
    ("review/technical.md", "Technical review (deck)"),
    ("review/product.md", "Product review (deck)"),
    ("review/leadership.md", "Leadership review (deck)"),
    ("review/customer-feedback.md", "Customer feedback document"),
];

/// The design documents in reading order (ADRs come after the workflows).
const DESIGN_ORDER: &[&str] = &[
    "design/architecture.md",
    "design/workflows.md",
    "design/adr/",
    "design/data-dictionary.md",
    "design/dataflow.md",
    "design/threat-model.md",
    "design/test-plan.md",
    "design/operations.md",
];

fn design_rank(path: &str) -> usize {
    DESIGN_ORDER
        .iter()
        .position(|p| path == *p || (p.ends_with('/') && path.starts_with(p)))
        .unwrap_or(DESIGN_ORDER.len())
}

/// The site that previews `sources`: a prototype alone opens directly; otherwise a
/// landing page links the prototype, the decks (as slide shows) and the design
/// documents (as pages).
pub fn site(sources: &Sources) -> Files {
    let mut files = Files::new();
    if !sources.keys().any(|p| is_doc(p)) {
        for (path, content) in sources {
            if let Some(rel) = path.strip_prefix(PROTOTYPE_DIR) {
                files.insert(rel.to_owned(), content.clone());
            }
        }
        return files;
    }
    let (mut reviews, mut designs) = (Vec::new(), Vec::new());
    for (path, content) in sources {
        if !is_doc(path) {
            files.insert(path.clone(), content.clone());
            continue;
        }
        let text = String::from_utf8_lossy(content);
        let page = format!("{}.html", path.trim_end_matches(".md"));
        let home = format!("{}index.html", "../".repeat(path.matches('/').count()));
        let stem = path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(".md");
        let title = render::title_of(&text).unwrap_or_else(|| stem.to_owned());
        let html = match render::deck(&text) {
            Some(slides) => render::deck_html(&title, &slides, &home),
            None => render::doc_html(&title, &text, &home),
        };
        files.insert(page.clone(), html.into_bytes());
        if path.starts_with("review/") {
            let order = REVIEWS.iter().position(|(p, _)| p == path);
            let label = order.map_or_else(|| title.clone(), |i| REVIEWS[i].1.to_owned());
            reviews.push((order.unwrap_or(REVIEWS.len()), label, page));
        } else {
            designs.push((design_rank(path), path.clone(), title, page));
        }
    }
    reviews.sort();
    designs.sort();
    let mut groups = Vec::new();
    if sources.contains_key("prototype/index.html") {
        groups.push((
            "UX prototype",
            vec![(
                "Click through the prototype".to_owned(),
                "prototype/index.html".to_owned(),
            )],
        ));
    }
    groups.push((
        "Reviews",
        reviews.into_iter().map(|(_, l, p)| (l, p)).collect(),
    ));
    groups.push((
        "Design",
        designs.into_iter().map(|(_, _, t, p)| (t, p)).collect(),
    ));
    files.insert(
        "index.html".into(),
        render::index_html("Review pack", &groups).into_bytes(),
    );
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

    /// File `path` of preview `cap` (`index.html` for a folder).
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

    const DECK_PATCH: &str = "diff --git a/review/leadership.md b/review/leadership.md\nnew file mode 100644\n--- /dev/null\n+++ b/review/leadership.md\n\
        @@ -0,0 +1,9 @@\n+---\n+marp: true\n+---\n+# Wishlist\n+<script>alert(1)</script>\n+---\n+## Summary\n+- Ship it\n+<!-- say why -->\n\
        diff --git a/design/notes.txt b/design/notes.txt\nnew file mode 100644\n--- /dev/null\n+++ b/design/notes.txt\n@@ -0,0 +1 @@\n+x\n\
        diff --git a/design/architecture.md b/design/architecture.md\nnew file mode 100644\n--- /dev/null\n+++ b/design/architecture.md\n@@ -0,0 +1 @@\n+# Architecture\n";

    #[test]
    fn previews_take_only_new_prototype_and_review_files_from_a_patch() {
        let files = sources_from_patch(PATCH);
        assert_eq!(
            files.keys().collect::<Vec<_>>(),
            ["prototype/app.js", "prototype/index.html"],
            "changed files, other folders and unsafe paths are left out"
        );
        assert_eq!(
            files["prototype/index.html"],
            b"<h1>Hi</h1>\n<script src=\"app.js\"></script>\n"
        );
        assert_eq!(files["prototype/app.js"], b"go()");
        assert_eq!(label(&files), Some("prototype"));
        let alone = site(&files);
        assert!(alone.contains_key("index.html") && alone.contains_key("app.js"));

        let both = sources_from_patches([PATCH, DECK_PATCH]);
        assert_eq!(
            both.keys().collect::<Vec<_>>(),
            [
                "design/architecture.md",
                "prototype/app.js",
                "prototype/index.html",
                "review/leadership.md"
            ],
            "only Markdown from design/ and review/"
        );
        assert_eq!(label(&both), Some("review pack"));
    }

    #[test]
    fn review_packs_render_decks_documents_and_the_prototype() {
        let files = site(&sources_from_patches([PATCH, DECK_PATCH]));
        let index = String::from_utf8_lossy(&files["index.html"]).into_owned();
        assert!(
            index.contains("href=\"prototype/index.html\"")
                && index.contains("href=\"review/leadership.html\">Leadership review (deck)")
                && index.contains("href=\"design/architecture.html\">Architecture"),
            "{index}"
        );
        let deck = String::from_utf8_lossy(&files["review/leadership.html"]).into_owned();
        assert_eq!(deck.matches("<section class=\"slide").count(), 2);
        assert!(deck.contains("href=\"../index.html\""));
        assert!(!deck.contains("<script>alert"), "raw HTML is shown as text");
        assert!(deck.contains("say why"), "speaker notes");
        assert!(files.contains_key("prototype/app.js"));
    }

    #[test]
    fn previews_are_served_by_capability_only_and_expire() {
        let previews = Previews::default();
        assert!(
            previews
                .add(Files::from([("a.html".into(), b"x".to_vec())]))
                .is_none()
        );
        let cap = previews.add(site(&sources_from_patch(PATCH))).unwrap();
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
        std::fs::create_dir_all(dir.path().join("design/adr")).unwrap();
        std::fs::write(dir.path().join("prototype/index.html"), "<p>ok</p>").unwrap();
        std::fs::write(dir.path().join("prototype/css/site.css"), "p{}").unwrap();
        std::fs::write(dir.path().join("prototype/big.bin"), vec![0u8; MAX_BYTES]).unwrap();
        std::fs::write(dir.path().join("design/adr/ADR-0001-x.md"), "# ADR-0001: X").unwrap();
        std::fs::write(dir.path().join("design/secret.txt"), "no").unwrap();
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
        let files = sources_from_dir(dir.path());
        assert!(
            files.contains_key("prototype/index.html")
                && files.contains_key("prototype/css/site.css")
                && files.contains_key("design/adr/ADR-0001-x.md"),
            "{:?}",
            files.keys()
        );
        assert!(
            !files.contains_key("design/secret.txt"),
            "only Markdown docs"
        );
        assert!(
            !files.contains_key("prototype/big.bin"),
            "it doesn't fit with the others"
        );
        assert!(files.values().map(Vec::len).sum::<usize>() <= MAX_BYTES);
        if linked {
            assert!(
                !files.contains_key("prototype/s.txt"),
                "links are not followed"
            );
        }
    }
}
