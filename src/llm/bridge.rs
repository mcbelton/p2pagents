use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

use super::{Access, LlmTask, Provider};
use crate::profile::INHERITED_CREDENTIAL_VARS;

type Pending = Arc<Mutex<Option<HashMap<u64, oneshot::Sender<Result<String, String>>>>>>;

#[derive(Serialize)]
struct Request<'a> {
    id: u64,
    provider: Provider,
    prompt: &'a str,
    cwd: &'a Path,
    access: Access,
    model: Option<&'a str>,
}

#[derive(Deserialize)]
struct Response {
    id: u64,
    ok: bool,
    #[serde(default)]
    text: String,
    #[serde(default)]
    error: String,
}

/// Client for `bridge/index.mjs`, a long-lived Node process that runs tasks
/// with the official Claude Agent SDK and OpenAI Codex SDK. Requests are
/// multiplexed by id, so several tasks can run concurrently.
pub struct Bridge {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicU64,
    _child: Child,
}

impl Bridge {
    /// Starts `node <script>` with inherited credential variables removed and
    /// `env` (the agent profile's config homes and secrets) set instead.
    pub fn spawn_node(script: &Path, env: Vec<(String, OsString)>) -> Result<Self> {
        if !script.is_file() {
            return Err(anyhow!(
                "bridge script {} not found; run `p2pagents setup` (needed for claude and codex)",
                script.display()
            ));
        }
        let mut cmd = Command::new("node");
        cmd.arg(script);
        for var in INHERITED_CREDENTIAL_VARS {
            cmd.env_remove(var);
        }
        cmd.envs(env);
        Self::spawn(cmd)
    }

    pub fn spawn(mut cmd: Command) -> Result<Self> {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .context("starting the Node bridge (is Node.js installed?)")?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));

        let reader_pending = pending.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(resp) = serde_json::from_str::<Response>(&line) else {
                    eprintln!("bridge: unexpected output: {line}");
                    continue;
                };
                let waiter = reader_pending
                    .lock()
                    .unwrap()
                    .as_mut()
                    .and_then(|p| p.remove(&resp.id));
                if let Some(waiter) = waiter {
                    let _ = waiter.send(if resp.ok {
                        Ok(resp.text)
                    } else {
                        Err(resp.error)
                    });
                }
            }
            // Bridge exited: dropping the waiters fails in-flight requests; `None` refuses new ones.
            reader_pending.lock().unwrap().take();
        });

        Ok(Self {
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicU64::new(1),
            _child: child,
        })
    }

    pub async fn run(&self, task: &LlmTask, cwd: &Path, access: Access) -> Result<String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap()
            .as_mut()
            .ok_or_else(|| anyhow!("bridge process exited"))?
            .insert(id, tx);

        let request = Request {
            id,
            provider: task.provider,
            prompt: &task.prompt,
            cwd,
            access,
            model: task.model.as_deref(),
        };
        let mut line = serde_json::to_vec(&request)?;
        line.push(b'\n');
        let written = {
            let mut stdin = self.stdin.lock().await;
            match stdin.write_all(&line).await {
                Ok(()) => stdin.flush().await,
                Err(e) => Err(e),
            }
        };
        if let Err(e) = written {
            if let Some(p) = self.pending.lock().unwrap().as_mut() {
                p.remove(&id);
            }
            return Err(e).context("writing to bridge");
        }

        match rx.await {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(error)) => Err(anyhow!(error)),
            Err(_) => Err(anyhow!("bridge process exited")),
        }
    }
}
