//! What a task changed in the workspace: a snapshot of file sizes and modification
//! times before and after the task, reported back with the result so the peer that
//! delegated a coding task can see which files it touched.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

use super::Access;

/// Folders not worth scanning (build output, dependencies, VCS data).
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "__pycache__"];
/// Stop scanning huge workspaces rather than stall the task.
const MAX_FILES: usize = 50_000;
/// Longest list put in a task result.
const MAX_LISTED: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    /// Path relative to the workspace, with `/` separators.
    pub path: String,
}

/// File sizes and modification times under a workspace.
#[derive(Debug, Default)]
pub struct Snapshot {
    files: BTreeMap<String, (u64, Option<SystemTime>)>,
    truncated: bool,
}

impl Snapshot {
    pub fn take(root: &Path) -> Self {
        let mut snap = Snapshot::default();
        let mut stack = vec![PathBuf::new()];
        while let Some(rel) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(root.join(&rel)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let rel = rel.join(&name);
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    if !SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                        stack.push(rel);
                    }
                } else if kind.is_file() {
                    if snap.files.len() >= MAX_FILES {
                        snap.truncated = true;
                        return snap;
                    }
                    let meta = entry.metadata().ok();
                    let key = rel.to_string_lossy().replace('\\', "/");
                    snap.files.insert(
                        key,
                        (
                            meta.as_ref().map_or(0, |m| m.len()),
                            meta.and_then(|m| m.modified().ok()),
                        ),
                    );
                }
            }
        }
        snap
    }

    /// Files added, modified or deleted between `self` and `after`.
    pub fn diff(&self, after: &Snapshot) -> Vec<Change> {
        let mut changes = Vec::new();
        for (path, meta) in &after.files {
            match self.files.get(path) {
                None => changes.push(Change {
                    kind: ChangeKind::Added,
                    path: path.clone(),
                }),
                Some(before) if before != meta => changes.push(Change {
                    kind: ChangeKind::Modified,
                    path: path.clone(),
                }),
                Some(_) => {}
            }
        }
        // A truncated "after" scan can't tell deleted files from unscanned ones.
        if !after.truncated {
            for path in self.files.keys().filter(|p| !after.files.contains_key(*p)) {
                changes.push(Change {
                    kind: ChangeKind::Deleted,
                    path: path.clone(),
                });
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        changes
    }
}

/// Markdown summary appended to a task result, or empty if nothing changed.
pub fn summary(changes: &[Change]) -> String {
    if changes.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "\n\n---\n**Files changed in the workspace ({}):**\n",
        changes.len()
    );
    for change in changes.iter().take(MAX_LISTED) {
        let mark = match change.kind {
            ChangeKind::Added => "added",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
        };
        out += &format!("- {mark} `{}`\n", change.path);
    }
    if changes.len() > MAX_LISTED {
        out += &format!("- ... and {} more\n", changes.len() - MAX_LISTED);
    }
    out
}

/// Runs `task` and, if `access` lets it edit files, appends the workspace changes it
/// made to its output. Changes by anything else running in the same workspace at
/// the same time are included too.
pub async fn track_changes<F>(workspace: &Path, access: Access, task: F) -> Result<String>
where
    F: Future<Output = Result<String>>,
{
    if access < Access::WorkspaceWrite {
        return task.await;
    }
    let root = workspace.to_path_buf();
    let before = tokio::task::spawn_blocking(move || Snapshot::take(&root)).await?;
    let output = task.await?;
    let root = workspace.to_path_buf();
    let after = tokio::task::spawn_blocking(move || Snapshot::take(&root)).await?;
    Ok(output + &summary(&before.diff(&after)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_added_modified_and_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        std::fs::create_dir_all(ws.join("src")).unwrap();
        std::fs::write(ws.join("src/lib.rs"), "a").unwrap();
        std::fs::write(ws.join("old.txt"), "x").unwrap();
        std::fs::write(ws.join("same.txt"), "y").unwrap();
        std::fs::create_dir_all(ws.join("target")).unwrap();

        let out = track_changes(&ws, Access::WorkspaceWrite, async {
            std::fs::write(ws.join("src/lib.rs"), "changed").unwrap();
            std::fs::write(ws.join("src/new.rs"), "n").unwrap();
            std::fs::remove_file(ws.join("old.txt")).unwrap();
            std::fs::write(ws.join("target/build.o"), "ignored").unwrap();
            Ok("done".to_owned())
        })
        .await
        .unwrap();

        assert!(out.starts_with("done\n\n---\n**Files changed in the workspace (3):**"));
        assert!(out.contains("- deleted `old.txt`"));
        assert!(out.contains("- modified `src/lib.rs`"));
        assert!(out.contains("- added `src/new.rs`"));
        assert!(!out.contains("same.txt") && !out.contains("build.o"));
    }

    #[tokio::test]
    async fn read_only_tasks_and_unchanged_workspaces_report_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        let out = track_changes(&ws, Access::ReadOnly, async {
            std::fs::write(ws.join("x"), "x").unwrap();
            Ok("answer".to_owned())
        })
        .await
        .unwrap();
        assert_eq!(out, "answer");
        let out = track_changes(&ws, Access::Full, async { Ok("answer".to_owned()) })
            .await
            .unwrap();
        assert_eq!(out, "answer");
    }

    #[test]
    fn long_lists_are_capped() {
        let changes: Vec<Change> = (0..60)
            .map(|i| Change {
                kind: ChangeKind::Added,
                path: format!("f{i}"),
            })
            .collect();
        let text = summary(&changes);
        assert_eq!(text.matches("- added").count(), MAX_LISTED);
        assert!(text.contains("... and 10 more"));
    }
}
