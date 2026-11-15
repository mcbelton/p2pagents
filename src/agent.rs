use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use iroh::endpoint::{Connection, RecvStream, SendStream, presets};
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot, watch};

use crate::llm::{Access, Executor, LlmTask, Provider};
use crate::protocol::{ALPN, Message, write_message};
use crate::trust::{TrustStore, TrustedPeer};

/// Maximum number of peer-delegated tasks an agent runs at once; more are queued.
pub const MAX_CONCURRENT_TASKS: usize = 2;
const HOUR: Duration = Duration::from_secs(3600);
/// How long a dropped peer may take to reconnect before `PeerDisconnected` is reported.
const DISCONNECT_GRACE: Duration = Duration::from_secs(2);

/// How the agent reaches peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Network {
    /// Over the internet: NAT hole punching, public relays as fallback, and
    /// lookup of peers by endpoint id (n0 relay and DNS services).
    #[default]
    Internet,
    /// Direct IP connections only (LAN, VPN, tests). No relays or lookup services.
    Local,
}

pub struct AgentConfig {
    /// Name this agent announces to peers.
    pub name: String,
    /// Identity; the public half is the agent's endpoint id.
    pub secret_key: SecretKey,
    pub network: Network,
    /// UDP socket to bind (default: any port on all interfaces).
    pub bind: Option<SocketAddr>,
    /// Peers this agent accepts connections from and dials.
    pub trust: TrustStore,
    /// Runs LLM tasks that allowlisted peers delegate (none: serve nothing).
    pub executor: Option<Arc<dyn Executor>>,
}

impl AgentConfig {
    pub fn new(name: impl Into<String>, secret_key: SecretKey) -> Self {
        Self {
            name: name.into(),
            secret_key,
            network: Network::Internet,
            bind: None,
            trust: TrustStore::default(),
            executor: None,
        }
    }
}

/// Notifications surfaced to the application embedding an [`Agent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    PeerConnected {
        name: String,
        id: EndpointId,
    },
    PeerDisconnected {
        name: String,
        id: EndpointId,
    },
    /// A connection from (or to) a key that is not on the allowlist was refused.
    PeerRejected {
        id: EndpointId,
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
    /// Local alias from the allowlist.
    pub name: String,
    pub id: EndpointId,
    pub capabilities: Vec<Provider>,
}

struct PeerHandle {
    conn_id: u64,
    name: String,
    addr: EndpointAddr,
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
    endpoint: Endpoint,
    trust: Mutex<TrustStore>,
    peers: Mutex<HashMap<EndpointId, PeerHandle>>,
    dialing: Mutex<HashSet<EndpointId>>,
    events: mpsc::UnboundedSender<Event>,
    next_conn_id: AtomicU64,
    shutdown: watch::Sender<bool>,
    executor: Option<Arc<dyn Executor>>,
    task_slots: Semaphore,
    task_log: Mutex<HashMap<EndpointId, VecDeque<Instant>>>,
    pending: Mutex<HashMap<u64, PendingTask>>,
    /// Peers reported via `PeerConnected` and not yet via `PeerDisconnected`.
    announced: Mutex<HashSet<EndpointId>>,
    next_task_id: AtomicU64,
}

/// A peer-to-peer agent on an iroh QUIC endpoint. Cheap to clone; all clones
/// share the same node.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<Inner>,
}

type Bound = (Agent, mpsc::UnboundedReceiver<Event>);

impl Agent {
    pub async fn bind(config: AgentConfig) -> Result<Bound> {
        let builder = match config.network {
            Network::Internet => Endpoint::builder(presets::N0),
            Network::Local => Endpoint::builder(presets::Minimal),
        };
        let mut builder = builder
            .secret_key(config.secret_key)
            .alpns(vec![ALPN.to_vec()]);
        if let Some(addr) = config.bind {
            builder = builder
                .bind_addr(addr)
                .map_err(|e| anyhow!("invalid bind address {addr}: {e}"))?;
        }
        let endpoint = builder.bind().await.context("starting the iroh endpoint")?;
        let (events, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            name: config.name,
            endpoint,
            trust: Mutex::new(config.trust),
            peers: Mutex::new(HashMap::new()),
            dialing: Mutex::new(HashSet::new()),
            events,
            next_conn_id: AtomicU64::new(0),
            shutdown: watch::Sender::new(false),
            executor: config.executor,
            task_slots: Semaphore::new(MAX_CONCURRENT_TASKS),
            task_log: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            announced: Mutex::new(HashSet::new()),
            next_task_id: AtomicU64::new(1),
        });
        tokio::spawn(accept_loop(inner.clone()));
        Ok((Self { inner }, rx))
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// This agent's public key; how peers identify and allowlist it.
    pub fn id(&self) -> EndpointId {
        self.inner.endpoint.id()
    }

    /// Current addresses (relay URL and/or IPs) other agents can dial.
    pub fn addr(&self) -> EndpointAddr {
        self.inner.endpoint.addr()
    }

    /// Waits until the endpoint is reachable through a relay (internet mode).
    pub async fn online(&self, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, self.inner.endpoint.online())
            .await
            .is_ok()
    }

    /// Providers this agent serves to peers (empty without an executor).
    pub fn capabilities(&self) -> Vec<Provider> {
        self.inner.capabilities()
    }

    /// Dials an allowlisted peer. Returns once the connection is established;
    /// the handshake completes asynchronously and is reported via [`Event::PeerConnected`].
    pub async fn connect(&self, addr: impl Into<EndpointAddr>) -> Result<()> {
        dial(self.inner.clone(), addr.into()).await
    }

    /// Dials every allowlisted peer that isn't connected, now and then every
    /// `interval`, so the mesh heals after restarts and network changes.
    pub fn keep_connected(&self, interval: Duration) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let mut shutdown = inner.shutdown.subscribe();
            loop {
                let trusted: Vec<TrustedPeer> = inner.trust.lock().unwrap().peers.clone();
                for peer in trusted {
                    let inner = inner.clone();
                    tokio::spawn(async move {
                        let _ = dial(inner, peer.addr()).await;
                    });
                }
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = shutdown.wait_for(|stopped| *stopped) => return,
                }
            }
        });
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        let mut peers: Vec<_> = self
            .inner
            .peers
            .lock()
            .unwrap()
            .iter()
            .map(|(id, p)| PeerInfo {
                name: p.name.clone(),
                id: *id,
                capabilities: p.capabilities.clone(),
            })
            .collect();
        peers.sort_by(|a, b| a.name.cmp(&b.name));
        peers
    }

    /// First connected peer (by name) that serves `provider`.
    pub fn find_peer(&self, provider: Provider) -> Option<PeerInfo> {
        self.peers()
            .into_iter()
            .find(|p| p.capabilities.contains(&provider))
    }

    /// The current allowlist.
    pub fn trusted(&self) -> TrustStore {
        self.inner.trust.lock().unwrap().clone()
    }

    /// Adds or updates an allowlisted peer and dials it. Persisting the allowlist
    /// is up to the caller.
    pub fn allow(&self, peer: TrustedPeer) {
        let addr = peer.addr();
        self.inner.trust.lock().unwrap().allow(peer);
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let _ = dial(inner, addr).await;
        });
    }

    /// Removes a peer (by name or id) from the allowlist and disconnects it.
    /// Returns whether it was on the allowlist.
    pub fn remove_trusted(&self, name_or_id: &str) -> bool {
        let ids: Vec<EndpointId> = {
            let mut trust = self.inner.trust.lock().unwrap();
            let ids = trust
                .peers
                .iter()
                .filter(|p| p.name == name_or_id || p.id.to_string() == name_or_id)
                .map(|p| p.id)
                .collect();
            trust.remove(name_or_id);
            ids
        };
        let peers = self.inner.peers.lock().unwrap();
        for id in &ids {
            if let Some(peer) = peers.get(id) {
                peer.close.notify_one();
            }
        }
        !ids.is_empty()
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

    /// Delegates an LLM task to a connected peer and waits for its result.
    /// Fails if the peer doesn't serve the provider, refuses the task, or disconnects.
    pub async fn delegate(&self, peer: EndpointId, task: LlmTask) -> Result<String> {
        let task_id = self.inner.next_task_id.fetch_add(1, Ordering::Relaxed);
        let (reply, rx) = oneshot::channel();
        {
            let peers = self.inner.peers.lock().unwrap();
            let handle = peers
                .get(&peer)
                .ok_or_else(|| anyhow!("not connected to {}", peer.fmt_short()))?;
            if !handle.capabilities.contains(&task.provider) {
                bail!("{} does not serve {}", handle.name, task.provider);
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
                bail!("connection to {} closed", handle.name);
            }
        }
        match rx.await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) => Err(anyhow!("{error}")),
            Err(_) => Err(anyhow!("delegated task was dropped")),
        }
    }

    /// Stops accepting peers, closes every connection and the endpoint.
    pub async fn shutdown(&self) {
        self.inner.shutdown.send_replace(true);
        for peer in self.inner.peers.lock().unwrap().values() {
            peer.close.notify_one();
        }
        self.inner.endpoint.close().await;
    }
}

async fn accept_loop(inner: Arc<Inner>) {
    let mut shutdown = inner.shutdown.subscribe();
    loop {
        let incoming = tokio::select! {
            incoming = inner.endpoint.accept() => incoming,
            _ = shutdown.wait_for(|stopped| *stopped) => return,
        };
        let Some(incoming) = incoming else { return };
        let inner = inner.clone();
        tokio::spawn(async move {
            let Ok(conn) = incoming.await else { return };
            let id = conn.remote_id();
            if !inner.is_trusted(&id) {
                let _ = inner.events.send(Event::PeerRejected { id });
                conn.close(1u32.into(), b"not on allowlist");
                return;
            }
            let Ok((send, recv)) = conn.accept_bi().await else {
                return;
            };
            let _ = run_connection(inner, conn, send, recv, false).await;
        });
    }
}

// Boxed because dial -> run_connection -> (gossip) dial is recursive.
fn dial(inner: Arc<Inner>, addr: EndpointAddr) -> Pin<Box<dyn Future<Output = Result<()>> + Send>> {
    Box::pin(async move {
        if *inner.shutdown.borrow() {
            bail!("agent is shut down");
        }
        let id = addr.id;
        if id == inner.endpoint.id() || inner.peers.lock().unwrap().contains_key(&id) {
            return Ok(());
        }
        if !inner.is_trusted(&id) {
            bail!(
                "{id} is not on the allowlist; add it with: p2pagents peers allow <ticket-or-id>"
            );
        }
        if !inner.dialing.lock().unwrap().insert(id) {
            return Ok(());
        }
        let result = async {
            let conn = inner.endpoint.connect(addr, ALPN).await?;
            let (send, recv) = conn.open_bi().await?;
            anyhow::Ok((conn, send, recv))
        }
        .await;
        inner.dialing.lock().unwrap().remove(&id);
        let (conn, send, recv) =
            result.with_context(|| format!("connecting to {}", id.fmt_short()))?;
        tokio::spawn(async move {
            let _ = run_connection(inner, conn, send, recv, true).await;
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

    fn is_trusted(&self, id: &EndpointId) -> bool {
        self.trust.lock().unwrap().get(id).is_some()
    }

    fn known_addrs(&self) -> Vec<EndpointAddr> {
        self.peers
            .lock()
            .unwrap()
            .values()
            .map(|p| p.addr.clone())
            .collect()
    }

    /// Registers a handshaken connection. If both sides dialed each other, both
    /// keep the connection initiated by the lower endpoint id.
    fn register(&self, id: EndpointId, handle: PeerHandle, outbound: bool) -> Registration {
        let me = self.endpoint.id();
        let mut peers = self.peers.lock().unwrap();
        let outcome = match peers.get(&id) {
            None => Registration::New,
            Some(old) => {
                let initiator = if outbound { me } else { id };
                if initiator != me.min(id) {
                    return Registration::Rejected;
                }
                old.close.notify_one();
                Registration::Replaced
            }
        };
        peers.insert(id, handle);
        outcome
    }

    fn unregister(&self, id: EndpointId, conn_id: u64) -> bool {
        let mut peers = self.peers.lock().unwrap();
        if peers.get(&id).is_some_and(|p| p.conn_id == conn_id) {
            peers.remove(&id);
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

    /// Checks the allowlist permission and hourly quota for a task from `id`;
    /// returns the access this peer is granted.
    fn admit_task(&self, id: &EndpointId) -> Result<Access> {
        let (name, can_delegate, per_hour, access) = {
            let trust = self.trust.lock().unwrap();
            let peer = trust
                .get(id)
                .ok_or_else(|| anyhow!("not on the allowlist"))?;
            (
                peer.name.clone(),
                peer.can_delegate,
                peer.tasks_per_hour,
                peer.access,
            )
        };
        if !can_delegate {
            bail!(
                "{} does not accept tasks from {name} (not allowed to delegate)",
                self.name
            );
        }
        let now = Instant::now();
        let mut log = self.task_log.lock().unwrap();
        let recent = log.entry(*id).or_default();
        while recent
            .front()
            .is_some_and(|t| now.duration_since(*t) > HOUR)
        {
            recent.pop_front();
        }
        if recent.len() >= per_hour as usize {
            bail!(
                "{} refused the task: {name} reached its limit of {per_hour} tasks per hour",
                self.name
            );
        }
        recent.push_back(now);
        Ok(access)
    }

    async fn serve_task(
        &self,
        id: &EndpointId,
        from: &str,
        task_id: u64,
        task: LlmTask,
    ) -> Result<String> {
        let executor = self
            .executor
            .as_ref()
            .ok_or_else(|| anyhow!("{} does not serve tasks", self.name))?;
        if !executor.providers().contains(&task.provider) {
            bail!("{} does not serve {}", self.name, task.provider);
        }
        let access = self.admit_task(id)?;
        let _ = self.events.send(Event::TaskReceived {
            from: from.to_owned(),
            task_id,
            provider: task.provider,
        });
        let _slot = self.task_slots.acquire().await?;
        let result = executor.execute(&task, access).await;
        let _ = self.events.send(Event::TaskCompleted {
            from: from.to_owned(),
            task_id,
            ok: result.is_ok(),
        });
        result
    }
}

async fn run_connection(
    inner: Arc<Inner>,
    conn: Connection,
    mut send: SendStream,
    recv: RecvStream,
    outbound: bool,
) -> Result<()> {
    let peer_id = conn.remote_id();
    let mut lines = BufReader::new(recv).lines();

    let hello = Message::Hello {
        name: inner.name.clone(),
        addr: inner.endpoint.addr(),
        capabilities: inner.capabilities(),
    };
    write_message(&mut send, &hello).await?;
    let first = lines
        .next_line()
        .await?
        .context("peer closed before hello")?;
    let Message::Hello {
        name: announced,
        addr,
        capabilities,
    } = serde_json::from_str(&first)?
    else {
        bail!("expected hello, got {first}");
    };
    // Gossiped addresses must belong to the authenticated peer.
    let addr = EndpointAddr::from_parts(peer_id, addr.addrs);
    let name = inner
        .trust
        .lock()
        .unwrap()
        .get(&peer_id)
        .map(|p| p.name.clone())
        .unwrap_or(announced);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let close = Arc::new(Notify::new());
    let conn_id = inner.next_conn_id.fetch_add(1, Ordering::Relaxed);
    let handle = PeerHandle {
        conn_id,
        name: name.clone(),
        addr,
        capabilities,
        tx: tx.clone(),
        close: close.clone(),
    };
    let known = inner.known_addrs();
    match inner.register(peer_id, handle, outbound) {
        Registration::Rejected => return Ok(()),
        Registration::Replaced => {}
        Registration::New => {
            // A peer that reconnects within the disconnect grace period was never
            // reported as gone, so it isn't reported as joining again either.
            if inner.announced.lock().unwrap().insert(peer_id) {
                let _ = inner.events.send(Event::PeerConnected {
                    name: name.clone(),
                    id: peer_id,
                });
            }
        }
    }
    // A shutdown that raced with this handshake missed our close handle.
    if *inner.shutdown.borrow() {
        close.notify_one();
    }

    let writer_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_message(&mut send, &msg).await.is_err() {
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
                    // Only allowlisted peers are dialed; dial() enforces it.
                    for addr in addrs.into_iter().filter(|a| inner.is_trusted(&a.id)) {
                        let inner = inner.clone();
                        tokio::spawn(async move {
                            let _ = dial(inner, addr).await;
                        });
                    }
                }
                Message::TaskRequest { task_id, task } => {
                    let (inner, tx, from) = (inner.clone(), tx.clone(), name.clone());
                    tokio::spawn(async move {
                        let (ok, output) =
                            match inner.serve_task(&peer_id, &from, task_id, task).await {
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
    conn.close(0u32.into(), b"bye");
    inner.fail_pending(conn_id);
    if inner.unregister(peer_id, conn_id) {
        // Report the disconnect only if the peer doesn't come back quickly, e.g.
        // when a connection loses the simultaneous-dial tie-break.
        let inner = inner.clone();
        tokio::spawn(async move {
            tokio::time::sleep(DISCONNECT_GRACE).await;
            let gone = !inner.peers.lock().unwrap().contains_key(&peer_id);
            if gone && inner.announced.lock().unwrap().remove(&peer_id) {
                let _ = inner
                    .events
                    .send(Event::PeerDisconnected { name, id: peer_id });
            }
        });
    }
    result
}
