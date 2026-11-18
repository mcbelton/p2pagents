//! Small JSON files a profile keeps (shared folders, schedules, plugins) are written
//! by the agent, its web UI and the CLI, which may run at the same time in separate
//! processes. Writes go to a temporary file renamed into place, so nobody reads a
//! half-written file; read-modify-write updates hold a lock file, so nobody's change
//! is lost.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};

/// How long a lock file may be held before it counts as left behind by a crash.
const STALE: Duration = Duration::from_secs(30);

/// Writes `bytes` to `path` atomically (a temporary file renamed over it).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("the file has no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(
        ".{name}.{}.tmp",
        data_encoding::HEXLOWER.encode(&iroh::SecretKey::generate().to_bytes()[..6])
    ));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    let mut tries = 0;
    loop {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            // Windows: a reader holding the file without delete sharing; retry briefly.
            Err(_) if tries < 20 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e).with_context(|| format!("writing {}", path.display()));
            }
        }
    }
}

/// An exclusive lock on a file (`<file>.lock`), released when dropped.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
}

impl FileLock {
    /// Waits up to 10 s for the lock on `target`; a lock older than 30 s was left
    /// behind by a crash and is taken over.
    pub fn acquire(target: &Path) -> Result<Self> {
        let mut name = target.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        let path = target.with_file_name(name);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        for _ in 0..200 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .is_some_and(|age| age > STALE);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("locking {}", target.display()));
                }
            }
        }
        bail!(
            "{} is being changed by another p2pagents process; try again",
            target.display()
        )
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_are_atomic_and_locks_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.json");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "two");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());

        let lock = FileLock::acquire(&file).unwrap();
        let waiter = std::thread::spawn({
            let file = file.clone();
            move || {
                let started = std::time::Instant::now();
                let _second = FileLock::acquire(&file).unwrap();
                started.elapsed()
            }
        });
        std::thread::sleep(Duration::from_millis(200));
        drop(lock);
        assert!(
            waiter.join().unwrap() >= Duration::from_millis(150),
            "waited for the first"
        );
        assert!(!dir.path().join("x.json.lock").exists(), "released");
    }
}
