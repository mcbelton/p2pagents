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

use crate::audit::{AuditEntry, AuditLog, Outcome};
use crate::llm::{Access, Executor, LlmTask, Provider, TaskContext};
use crate::protocol::{ALPN, Message, write_message};
use crate::shares::{DATA_SHARE, ShareInfo, ShareStore, effective_access};
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
    /// Where to record tasks peers ask this agent to run (none: no audit log).
    pub audit: Option<Arc<AuditLog>>,
    /// Folders peers' tasks may run in (each peer only in those granted to it).
    pub shares: ShareStore,
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
            audit: None,
            shares: ShareStore::default(),
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
    /// A task received from a peer finished (done, failed or cancelled) and its
    /// result was sent back.
    TaskCompleted {
        from: String,
        task_id: u64,
        outcome: Outcome,
    },
    /// A peer's task was not run (not allowed to delegate, over its hourly limit,
    /// or a provider this agent doesn't serve).
    TaskRefused {
        from: String,
        task_id: u64,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    /// Local alias from the allowlist.
    pub name: String,
    pub id: EndpointId,
    pub capabilities: Vec<Provider>,
    /// Folders this peer shares with us for tasks (name and access we get).
    pub shares: Vec<ShareInfo>,
}

struct PeerHandle {
    conn_id: u64,
    name: String,
    addr: EndpointAddr,
    capabilities: Vec<Provider>,
    shares: Vec<ShareInfo>,
    tx: mpsc::UnboundedSender<Message>,
    close: Arc<Notify>,
}

struct PendingTask {
    conn_id: u64,
    reply: oneshot::Sender<Result<String, String>>,
}

/// Sends `TaskCancel` to the peer if a delegated task's future is dropped before
/// the result arrives.
struct CancelOnDrop<'a> {
    inner: &'a Inner,
    peer: EndpointId,
    task_id: u64,
    armed: bool,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.inner.pending.lock().unwrap().remove(&self.task_id);
        if let Some(handle) = self.inner.peers.lock().unwrap().get(&self.peer) {
            let _ = handle.tx.send(Message::TaskCancel {
                task_id: self.task_id,
            });
        }
    }
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
    audit: Option<Arc<AuditLog>>,
    shares: Mutex<ShareStore>,
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
            audit: config.audit,
            shares: Mutex::new(config.shares),
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

    /// Folders this agent shares (peers see only those granted to them).
    pub fn shares(&self) -> ShareStore {
        self.inner.shares.lock().unwrap().clone()
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
                shares: p.shares.clone(),
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
        let (id, addr) = (peer.id, peer.addr());
        self.inner.trust.lock().unwrap().allow(peer);
        // A connected peer learns its new folder grants right away.
        let shares = self.inner.shares_for(&id);
        if let Some(handle) = self.inner.peers.lock().unwrap().get(&id) {
            let _ = handle.tx.send(Message::Shares { shares });
        }
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
        // Dropping this future (e.g. the user cancels) tells the peer to stop the task.
        let mut guard = CancelOnDrop {
            inner: &self.inner,
            peer,
            task_id,
            armed: true,
        };
        let result = rx.await;
        guard.armed = false;
        match result {
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
    /// Folders `peer` may run tasks in, by name and access (only if it may delegate
    /// and this agent serves tasks).
    fn shares_for(&self, peer: &EndpointId) -> Vec<ShareInfo> {
        if self.executor.is_none() {
            return Vec::new();
        }
        let trust = self.trust.lock().unwrap();
        match trust.get(peer) {
            Some(p) if p.can_delegate => self.shares.lock().unwrap().visible(&p.shares, p.access),
            _ => Vec::new(),
        }
    }

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

    /// Checks the allowlist permission, the shared folder and the hourly quota for
    /// a task from `id`; returns where and with what access it may run.
    fn admit_task(&self, id: &EndpointId, task: &LlmTask) -> Result<TaskContext> {
        let (name, can_delegate, per_hour, grant, granted_shares) = {
            let trust = self.trust.lock().unwrap();
            let peer = trust
                .get(id)
                .ok_or_else(|| anyhow!("not on the allowlist"))?;
            (
                peer.name.clone(),
                peer.can_delegate,
                peer.tasks_per_hour,
                peer.access,
                peer.shares.clone(),
            )
        };
        if !can_delegate {
            bail!(
                "{} does not accept tasks from {name} (not allowed to delegate)",
                self.name
            );
        }
        let share_name = task.share.as_deref().unwrap_or(DATA_SHARE);
        let share = self
            .shares
            .lock()
            .unwrap()
            .get(share_name)
            .filter(|_| granted_shares.iter().any(|s| s == share_name))
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "{} has not shared a folder named '{share_name}' with {name}",
                    self.name
                )
            })?;
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
        Ok(TaskContext {
            access: effective_access(grant, share.max_access),
            workspace: share.path,
        })
    }

    /// Runs a peer's task until it finishes or `cancel` fires (the peer sent
    /// `TaskCancel` or disconnected), then audits it and reports the outcome.
    async fn serve_task(
        &self,
        id: &EndpointId,
        from: &str,
        task_id: u64,
        task: LlmTask,
        cancel: oneshot::Receiver<()>,
    ) -> Result<String> {
        let clock = std::time::SystemTime::now();
        let mut granted = None;
        let mut cancelled = false;
        let result = self
            .run_peer_task(
                id,
                from,
                task_id,
                &task,
                cancel,
                &mut granted,
                &mut cancelled,
            )
            .await;
        let mut entry = AuditEntry::new(from, id, task_id, &task, granted, &result, clock);
        if cancelled {
            entry.outcome = Outcome::Cancelled;
        }
        if let Some(audit) = &self.audit {
            audit.record(&entry);
        }
        let event = match (&result, entry.outcome) {
            (Err(e), Outcome::Refused) => Event::TaskRefused {
                from: from.to_owned(),
                task_id,
                reason: format!("{e:#}"),
            },
            (_, outcome) => Event::TaskCompleted {
                from: from.to_owned(),
                task_id,
                outcome,
            },
        };
        let _ = self.events.send(event);
        result
    }

    /// Checks and runs a peer's task; sets `granted` once it is admitted.
    #[allow(clippy::too_many_arguments)]
    async fn run_peer_task(
        &self,
        id: &EndpointId,
        from: &str,
        task_id: u64,
        task: &LlmTask,
        cancel: oneshot::Receiver<()>,
        granted: &mut Option<Access>,
        cancelled: &mut bool,
    ) -> Result<String> {
        let executor = self
            .executor
            .as_ref()
            .ok_or_else(|| anyhow!("{} does not serve tasks", self.name))?;
        if !executor.providers().contains(&task.provider) {
            bail!("{} does not serve {}", self.name, task.provider);
        }
        let ctx = self.admit_task(id, task)?;
        *granted = Some(ctx.access);
        let _ = self.events.send(Event::TaskReceived {
            from: from.to_owned(),
            task_id,
            provider: task.provider,
        });
        let work = async {
            let _slot = self.task_slots.acquire().await?;
            executor.execute(task, &ctx).await
        };
        // Dropping `work` stops the provider run (each backend aborts on drop).
        tokio::select! {
            result = work => result,
            _ = cancel => {
                *cancelled = true;
                Err(anyhow!("task cancelled by {from}"))
            }
        }
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
        shares: inner.shares_for(&peer_id),
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
        shares,
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
        shares,
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
    // Cancel senders of this peer's running tasks; dropping one cancels its task.
    let running: Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>> = Arc::default();

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
                    let (cancel_tx, cancel_rx) = oneshot::channel();
                    running.lock().unwrap().insert(task_id, cancel_tx);
                    let running = running.clone();
                    tokio::spawn(async move {
                        let result = inner
                            .serve_task(&peer_id, &from, task_id, task, cancel_rx)
                            .await;
                        running.lock().unwrap().remove(&task_id);
                        let (ok, output) = match result {
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
                Message::TaskCancel { task_id } => {
                    if let Some(cancel) = running.lock().unwrap().remove(&task_id) {
                        let _ = cancel.send(());
                    }
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
                Message::Shares { shares } => {
                    if let Some(handle) = inner.peers.lock().unwrap().get_mut(&peer_id)
                        && handle.conn_id == conn_id
                    {
                        handle.shares = shares;
                    }
                }
                Message::Hello { .. } => bail!("unexpected second hello"),
                Message::Unknown => {}
            }
        }
        Ok(())
    }
    .await;

    // Nobody is left to receive the results: stop the peer's running tasks.
    running.lock().unwrap().clear();
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
