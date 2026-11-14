use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream, ToSocketAddrs};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot, watch};

use crate::llm::{Executor, LlmTask, Provider};
use crate::protocol::{Message, write_message};

/// Maximum number of peer-delegated tasks an agent runs at once; more are queued.
pub const MAX_CONCURRENT_TASKS: usize = 2;

/// Notifications surfaced to the application embedding an [`Agent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PeerConnected {
        name: String,
        addr: SocketAddr,
    },
    PeerDisconnected {
        name: String,
        addr: SocketAddr,
    },
    Chat {
        from: String,
        text: String,
    },
    /// A peer delegated a task to this agent and it was accepted.
    TaskReceived {
        from: String,
        task_id: u64,
        provider: Provider,
    },
    /// A task received from a peer finished and its result was sent back.
    TaskCompleted {
        from: String,
        task_id: u64,
        ok: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub name: String,
    pub addr: SocketAddr,
    pub capabilities: Vec<Provider>,
}

struct PeerHandle {
    conn_id: u64,
    name: String,
    capabilities: Vec<Provider>,
    tx: mpsc::UnboundedSender<Message>,
    close: Arc<Notify>,
}

struct PendingTask {
    conn_id: u64,
    reply: oneshot::Sender<Result<String, String>>,
}

struct Inner {
    name: String,
    listen_addr: SocketAddr,
    peers: Mutex<HashMap<SocketAddr, PeerHandle>>,
    dialing: Mutex<HashSet<SocketAddr>>,
    events: mpsc::UnboundedSender<Event>,
    next_conn_id: AtomicU64,
    shutdown: watch::Sender<bool>,
    executor: Option<Arc<dyn Executor>>,
    task_slots: Semaphore,
    pending: Mutex<HashMap<u64, PendingTask>>,
    next_task_id: AtomicU64,
}

/// A peer-to-peer agent. Cheap to clone; all clones share the same node.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<Inner>,
}

type Bound = (Agent, mpsc::UnboundedReceiver<Event>);

impl Agent {
    /// Binds a listener and starts accepting peers. Bind to a concrete IP
    /// (not 0.0.0.0) because the bound address is what gets advertised to peers.
    pub async fn bind(name: impl Into<String>, addr: impl ToSocketAddrs) -> Result<Bound> {
        Self::bind_inner(name.into(), addr, None).await
    }

    /// Like [`Agent::bind`], but also runs LLM tasks delegated by peers with
    /// `executor`, and advertises its providers to them.
    pub async fn bind_with_executor(
        name: impl Into<String>,
        addr: impl ToSocketAddrs,
        executor: Arc<dyn Executor>,
    ) -> Result<Bound> {
        Self::bind_inner(name.into(), addr, Some(executor)).await
    }

    async fn bind_inner(
        name: String,
        addr: impl ToSocketAddrs,
        executor: Option<Arc<dyn Executor>>,
    ) -> Result<Bound> {
        let listener = TcpListener::bind(addr).await.context("binding listener")?;
        let (events, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            name,
            listen_addr: listener.local_addr()?,
            peers: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashSet::new()),
            events,
            next_conn_id: AtomicU64::new(0),
            shutdown: watch::Sender::new(false),
            executor,
            task_slots: Semaphore::new(MAX_CONCURRENT_TASKS),
            pending: Mutex::new(HashMap::new()),
            next_task_id: AtomicU64::new(1),
        });
        tokio::spawn(accept_loop(inner.clone(), listener));
        Ok((Self { inner }, rx))
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.inner.listen_addr
    }

    /// Providers this agent serves to peers (empty without an executor).
    pub fn capabilities(&self) -> Vec<Provider> {
        self.inner.capabilities()
    }

    /// Dials a peer. Returns once the TCP connection is established; the
    /// handshake completes asynchronously and is reported via [`Event::PeerConnected`].
    pub async fn connect(&self, addr: SocketAddr) -> Result<()> {
        dial(self.inner.clone(), addr).await
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        let mut peers: Vec<_> = self
            .inner
            .peers
            .lock()
            .unwrap()
            .iter()
            .map(|(addr, p)| PeerInfo {
                name: p.name.clone(),
                addr: *addr,
                capabilities: p.capabilities.clone(),
            })
            .collect();
        peers.sort_by_key(|p| p.addr);
        peers
    }

    /// First connected peer (by address) that serves `provider`.
    pub fn find_peer(&self, provider: Provider) -> Option<PeerInfo> {
        self.peers()
            .into_iter()
            .find(|p| p.capabilities.contains(&provider))
    }

    /// Sends a chat message to every connected peer; returns how many it was sent to.
    pub fn broadcast(&self, text: impl Into<String>) -> usize {
        let msg = Message::Chat {
            from: self.inner.name.clone(),
            text: text.into(),
        };
        self.inner
            .peers
            .lock()
            .unwrap()
            .values()
            .filter(|p| p.tx.send(msg.clone()).is_ok())
            .count()
    }

    /// Delegates an LLM task to the peer at `peer` and waits for its result.
    /// Fails if the peer doesn't serve the provider, rejects the task, or disconnects.
    pub async fn delegate(&self, peer: SocketAddr, task: LlmTask) -> Result<String> {
        let task_id = self.inner.next_task_id.fetch_add(1, Ordering::Relaxed);
        let (reply, rx) = oneshot::channel();
        {
            let peers = self.inner.peers.lock().unwrap();
            let handle = peers
                .get(&peer)
                .ok_or_else(|| anyhow!("not connected to {peer}"))?;
            if !handle.capabilities.contains(&task.provider) {
                bail!("{} ({peer}) does not serve {}", handle.name, task.provider);
            }
            self.inner.pending.lock().unwrap().insert(
                task_id,
                PendingTask {
                    conn_id: handle.conn_id,
                    reply,
                },
            );
            if handle
                .tx
                .send(Message::TaskRequest { task_id, task })
                .is_err()
            {
                self.inner.pending.lock().unwrap().remove(&task_id);
                bail!("connection to {peer} closed");
            }
        }
        match rx.await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(anyhow!("{error}")),
            Err(_) => Err(anyhow!("delegated task was dropped")),
        }
    }

    /// Stops accepting peers and closes every connection. Peers observe a disconnect.
    pub fn shutdown(&self) {
        self.inner.shutdown.send_replace(true);
        for peer in self.inner.peers.lock().unwrap().values() {
            peer.close.notify_one();
        }
    }
}

async fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    let mut shutdown = inner.shutdown.subscribe();
    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = shutdown.wait_for(|stopped| *stopped) => return,
        };
        match accepted {
            Ok((stream, _)) => {
                let inner = inner.clone();
                tokio::spawn(async move {
                    let _ = run_connection(inner, stream, false).await;
                });
            }
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
}

// Boxed because dial -> run_connection -> (gossip) dial is recursive.
fn dial(inner: Arc<Inner>, addr: SocketAddr) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(async move {
        if *inner.shutdown.borrow() {
            bail!("agent is shut down");
        }
        if addr == inner.listen_addr || inner.peers.lock().unwrap().contains_key(&addr) {
            return Ok(());
        }
        if !inner.dialing.lock().unwrap().insert(addr) {
            return Ok(());
        }
        let result = TcpStream::connect(addr).await;
        inner.dialing.lock().unwrap().remove(&addr);
        let stream = result.with_context(|| format!("connecting to {addr}"))?;
        tokio::spawn(async move {
            let _ = run_connection(inner, stream, true).await;
        });
        Ok(())
    })
}

enum Registration {
    New,
    Replaced,
    Rejected,
}

impl Inner {
    fn capabilities(&self) -> Vec<Provider> {
        self.executor
            .as_ref()
            .map(|e| e.providers())
            .unwrap_or_default()
    }

    fn known_addrs(&self) -> Vec<SocketAddr> {
        self.peers.lock().unwrap().keys().copied().collect()
    }

    /// Registers a handshaken connection. If a connection to the same peer
    /// already exists (both sides dialed each other), both nodes keep the one
    /// initiated by the lower listen address so they converge on the same link.
    fn register(&self, addr: SocketAddr, handle: PeerHandle, outbound: bool) -> Registration {
        let mut peers = self.peers.lock().unwrap();
        let outcome = match peers.get(&addr) {
            None => Registration::New,
            Some(old) => {
                let initiator = if outbound { self.listen_addr } else { addr };
                if initiator != self.listen_addr.min(addr) {
                    return Registration::Rejected;
                }
                old.close.notify_one();
                Registration::Replaced
            }
        };
        peers.insert(addr, handle);
        outcome
    }

    fn unregister(&self, addr: SocketAddr, conn_id: u64) -> bool {
        let mut peers = self.peers.lock().unwrap();
        if peers.get(&addr).is_some_and(|p| p.conn_id == conn_id) {
            peers.remove(&addr);
            return true;
        }
        false
    }

    /// Fails tasks delegated over a connection that has closed.
    fn fail_pending(&self, conn_id: u64) {
        let mut pending = self.pending.lock().unwrap();
        let ids: Vec<u64> = pending
            .iter()
            .filter(|(_, p)| p.conn_id == conn_id)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(task) = pending.remove(&id) {
                let _ = task
                    .reply
                    .send(Err("peer disconnected before returning a result".into()));
            }
        }
    }

    fn complete_pending(&self, conn_id: u64, task_id: u64, result: Result<String, String>) {
        let mut pending = self.pending.lock().unwrap();
        // Only the peer the task was sent to may answer it.
        if pending.get(&task_id).is_some_and(|p| p.conn_id == conn_id)
            && let Some(task) = pending.remove(&task_id)
        {
            let _ = task.reply.send(result);
        }
    }

    async fn serve_task(&self, from: &str, task_id: u64, task: LlmTask) -> Result<String> {
        let executor = self
            .executor
            .as_ref()
            .ok_or_else(|| anyhow!("{} does not serve tasks", self.name))?;
        if !executor.providers().contains(&task.provider) {
            bail!("{} does not serve {}", self.name, task.provider);
        }
        let _ = self.events.send(Event::TaskReceived {
            from: from.to_owned(),
            task_id,
            provider: task.provider,
        });
        let _slot = self.task_slots.acquire().await?;
        let result = executor.execute(&task).await;
        let _ = self.events.send(Event::TaskCompleted {
            from: from.to_owned(),
            task_id,
            ok: result.is_ok(),
        });
        result
    }
}

async fn run_connection(inner: Arc<Inner>, stream: TcpStream, outbound: bool) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let hello = Message::Hello {
        name: inner.name.clone(),
        listen_addr: inner.listen_addr,
        capabilities: inner.capabilities(),
    };
    write_message(&mut writer, &hello).await?;
    let first = lines
        .next_line()
        .await?
        .context("peer closed before hello")?;
    let Message::Hello {
        name: peer_name,
        listen_addr: peer_addr,
        capabilities,
    } = serde_json::from_str(&first)?
    else {
        bail!("expected hello, got {first}");
    };
    if peer_addr == inner.listen_addr {
        return Ok(());
    }

    let (tx, mut rx) = mpsc::unbounded_channel();
    let close = Arc::new(Notify::new());
    let conn_id = inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
    let handle = PeerHandle {
        conn_id,
        name: peer_name.clone(),
        capabilities,
        tx: tx.clone(),
        close: close.clone(),
    };
    let known = inner.known_addrs();
    match inner.register(peer_addr, handle, outbound) {
        Registration::Rejected => return Ok(()),
        Registration::Replaced => {}
        Registration::New => {
            let _ = inner.events.send(Event::PeerConnected {
                name: peer_name.clone(),
                addr: peer_addr,
            });
        }
    }
    // A shutdown that raced with this handshake missed our close handle.
    if *inner.shutdown.borrow() {
        close.notify_one();
    }

    let writer_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_message(&mut writer, &msg).await.is_err() {
                break;
            }
        }
    });
    let _ = tx.send(Message::Peers { addrs: known });

    let result = async {
        loop {
            let line = tokio::select! {
                line = lines.next_line() => line?,
                _ = close.notified() => break,
            };
            let Some(line) = line else { break };
            match serde_json::from_str::<Message>(&line)? {
                Message::Chat { from, text } => {
                    let _ = inner.events.send(Event::Chat { from, text });
                }
                Message::Peers { addrs } => {
                    for addr in addrs {
                        let inner = inner.clone();
                        tokio::spawn(async move {
                            let _ = dial(inner, addr).await;
                        });
                    }
                }
                Message::TaskRequest { task_id, task } => {
                    let (inner, tx, from) = (inner.clone(), tx.clone(), peer_name.clone());
                    tokio::spawn(async move {
                        let (ok, output) = match inner.serve_task(&from, task_id, task).await {
                            Ok(output) => (true, output),
                            Err(e) => (false, format!("{e:#}")),
                        };
                        let _ = tx.send(Message::TaskResult {
                            task_id,
                            ok,
                            output,
                        });
                    });
                }
                Message::TaskResult {
                    task_id,
                    ok,
                    output,
                } => {
                    inner.complete_pending(
                        conn_id,
                        task_id,
                        if ok { Ok(output) } else { Err(output) },
                    );
                }
                Message::Hello { .. } => bail!("unexpected second hello"),
            }
        }
        Ok(())
    }
    .await;

    writer_task.abort();
    inner.fail_pending(conn_id);
    if inner.unregister(peer_addr, conn_id) {
        let _ = inner.events.send(Event::PeerDisconnected {
            name: peer_name,
            addr: peer_addr,
        });
    }
    result
}
