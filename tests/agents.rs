//! Multi-agent tests over real iroh QUIC endpoints on localhost (`Network::Local`:
//! direct IPs only, no relays or lookup services, so no internet is needed).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use p2pagents::audit::{AuditLog, Outcome};
use p2pagents::shares::{DATA_SHARE, ShareInfo, ShareStore};
use p2pagents::{
    Access, Agent, AgentConfig, EndpointId, Event, Executor, LlmTask, Network, Provider, SecretKey,
    TaskContext, TrustStore, TrustedPeer,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

/// A node's built-in data folder (the fake executor never touches it).
fn data_folder(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join("p2p-test-data").join(name)
}

/// Echoes the prompt; "hang" never completes (and counts in `dropped` when its run
/// is dropped, i.e. stopped), "fail" errors, "access" returns the access the agent
/// granted the task and "where" the folder it runs in.
struct FakeExecutor {
    providers: Vec<Provider>,
    dropped: Arc<AtomicUsize>,
}

/// Increments the counter when dropped.
struct CountDrop(Arc<AtomicUsize>);

impl Drop for CountDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Executor for FakeExecutor {
    fn providers(&self) -> Vec<Provider> {
        self.providers.clone()
    }

    async fn execute(&self, task: &LlmTask, ctx: &TaskContext) -> anyhow::Result<String> {
        match task.prompt.as_str() {
            "hang" => {
                let _count = CountDrop(self.dropped.clone());
                std::future::pending().await
            }
            "fail" => anyhow::bail!("model error"),
            "access" => Ok(ctx.access.to_string()),
            "where" => Ok(ctx.workspace.display().to_string()),
            prompt => Ok(format!("{} did: {prompt}", task.provider)),
        }
    }
}

struct Node {
    name: &'static str,
    key: SecretKey,
    trust: TrustStore,
    executor: Option<Arc<dyn Executor>>,
    audit: Option<Arc<AuditLog>>,
    /// Hanging task runs that were stopped (see [`FakeExecutor`]).
    stopped: Arc<AtomicUsize>,
    shares: ShareStore,
}

impl Node {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            key: SecretKey::generate(),
            trust: TrustStore::default(),
            executor: None,
            audit: None,
            stopped: Arc::default(),
            shares: ShareStore::with_data(data_folder(name)),
        }
    }

    fn id(&self) -> EndpointId {
        self.key.public()
    }

    fn serving(mut self, providers: Vec<Provider>) -> Self {
        self.executor = Some(Arc::new(FakeExecutor {
            providers,
            dropped: self.stopped.clone(),
        }));
        self
    }

    /// Allowlists `other`, optionally letting it delegate up to `per_hour` tasks.
    fn trusts(&mut self, other: &Node, delegate: Option<u32>) {
        self.trust.allow(TrustedPeer {
            name: other.name.into(),
            id: other.id(),
            addrs: BTreeSet::new(),
            can_delegate: delegate.is_some(),
            tasks_per_hour: delegate.unwrap_or(0),
            access: Access::ReadOnly,
            shares: vec![DATA_SHARE.to_owned()],
        });
    }

    async fn start(self) -> (Agent, UnboundedReceiver<Event>) {
        let mut config = AgentConfig::new(self.name, self.key);
        config.network = Network::Local;
        config.bind = Some("127.0.0.1:0".parse().unwrap());
        config.trust = self.trust;
        config.executor = self.executor;
        config.audit = self.audit;
        config.shares = self.shares;
        Agent::bind(config).await.unwrap()
    }
}

fn mutual(a: &mut Node, b: &mut Node) {
    a.trusts(b, Some(100));
    b.trusts(a, Some(100));
}

async fn next_matching(
    events: &mut UnboundedReceiver<Event>,
    pred: impl Fn(&Event) -> bool,
) -> Event {
    timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.expect("event channel closed");
            if pred(&event) {
                return event;
            }
        }
    })
    .await
    .expect("timed out waiting for event")
}

async fn wait_for_peers(agent: &Agent, count: usize) {
    timeout(Duration::from_secs(10), async {
        while agent.peers().len() != count {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "{} has {:?}, expected {count} peers",
            agent.name(),
            agent.peers()
        )
    });
}

#[tokio::test]
async fn peers_discover_each_other_via_gossip_and_chat() {
    let (mut a, mut b, mut c) = (Node::new("a"), Node::new("b"), Node::new("c"));
    mutual(&mut a, &mut b);
    mutual(&mut a, &mut c);
    mutual(&mut b, &mut c);
    let (a, _ae) = a.start().await;
    let (b, _be) = b.start().await;
    let (c, mut c_events) = c.start().await;

    b.connect(a.addr()).await.unwrap();
    wait_for_peers(&a, 1).await;
    // c only dials a; it learns b's address from a's gossip.
    c.connect(a.addr()).await.unwrap();
    for agent in [&a, &b, &c] {
        wait_for_peers(agent, 2).await;
    }

    assert_eq!(b.broadcast("hello"), 2);
    let event = next_matching(&mut c_events, |e| matches!(e, Event::Chat { .. })).await;
    assert_eq!(
        event,
        Event::Chat {
            from: "b".into(),
            text: "hello".into()
        }
    );
}

#[tokio::test]
async fn rejects_peers_that_are_not_allowlisted() {
    let mut a = Node::new("a");
    let mut stranger = Node::new("stranger");
    stranger.trusts(&a, None);
    let a_id = a.id();
    let mut b = Node::new("b");
    a.trusts(&b, None);
    let (a, mut a_events) = a.start().await;
    let (stranger, _se) = stranger.start().await;

    // stranger trusts a, but a does not trust stranger: a drops the connection.
    let _ = stranger.connect(a.addr()).await;
    let event = next_matching(&mut a_events, |e| matches!(e, Event::PeerRejected { .. })).await;
    assert_eq!(event, Event::PeerRejected { id: stranger.id() });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(a.peers().is_empty());
    assert!(stranger.peers().is_empty());

    // An agent refuses to dial keys it has not allowlisted.
    b.trust = TrustStore::default();
    let (b, _be) = b.start().await;
    let err = b.connect(a.addr()).await.unwrap_err();
    assert!(err.to_string().contains("not on the allowlist"), "{err}");
    assert_eq!(a.id(), a_id);
}

#[tokio::test]
async fn gossip_does_not_connect_peers_that_do_not_trust_each_other() {
    let (mut a, mut b, mut c) = (Node::new("a"), Node::new("b"), Node::new("c"));
    mutual(&mut a, &mut b);
    mutual(&mut a, &mut c);
    let (a, _ae) = a.start().await;
    let (b, _be) = b.start().await;
    let (c, _ce) = c.start().await;

    b.connect(a.addr()).await.unwrap();
    wait_for_peers(&a, 1).await;
    c.connect(a.addr()).await.unwrap();
    wait_for_peers(&a, 2).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(b.peers().len(), 1);
    assert_eq!(c.peers().len(), 1);
}

#[tokio::test]
async fn simultaneous_dials_converge_on_one_connection() {
    let (mut a, mut b) = (Node::new("a"), Node::new("b"));
    mutual(&mut a, &mut b);
    let (a, _ae) = a.start().await;
    let (b, mut b_events) = b.start().await;

    let (ra, rb) = tokio::join!(a.connect(b.addr()), b.connect(a.addr()));
    ra.unwrap();
    rb.unwrap();
    wait_for_peers(&a, 1).await;
    wait_for_peers(&b, 1).await;
    // Longer than the disconnect grace period, so a spurious leave would show up.
    tokio::time::sleep(Duration::from_millis(2500)).await;

    let mut joins = 0;
    while let Ok(event) = b_events.try_recv() {
        assert!(
            !matches!(event, Event::PeerDisconnected { .. }),
            "dial race reported a disconnect"
        );
        joins += matches!(event, Event::PeerConnected { .. }) as usize;
    }
    assert_eq!(joins, 1, "dial race reported more than one join");
    assert_eq!(a.peers().len(), 1);
    assert_eq!(b.peers().len(), 1);
    assert_eq!(a.broadcast("ping"), 1);
    let event = next_matching(&mut b_events, |e| matches!(e, Event::Chat { .. })).await;
    assert_eq!(
        event,
        Event::Chat {
            from: "a".into(),
            text: "ping".into()
        }
    );
}

#[tokio::test]
async fn shutdown_disconnects_peers() {
    let (mut a, mut b) = (Node::new("a"), Node::new("b"));
    mutual(&mut a, &mut b);
    let b_id = b.id();
    let (a, mut a_events) = a.start().await;
    let (b, _be) = b.start().await;

    b.connect(a.addr()).await.unwrap();
    next_matching(&mut a_events, |e| matches!(e, Event::PeerConnected { .. })).await;

    b.shutdown().await;
    let event = next_matching(&mut a_events, |e| {
        matches!(e, Event::PeerDisconnected { .. })
    })
    .await;
    assert_eq!(
        event,
        Event::PeerDisconnected {
            name: "b".into(),
            id: b_id
        }
    );
    assert!(a.peers().is_empty());
    assert!(b.connect(a.addr()).await.is_err());
}

#[tokio::test]
async fn delegates_tasks_to_a_peer_that_serves_the_provider() {
    let mut worker = Node::new("worker").serving(vec![Provider::Claude, Provider::Codex]);
    let mut boss = Node::new("boss");
    mutual(&mut worker, &mut boss);
    let (worker, mut worker_events) = worker.start().await;
    let (boss, _be) = boss.start().await;
    boss.connect(worker.addr()).await.unwrap();
    wait_for_peers(&boss, 1).await;

    let peer = boss
        .find_peer(Provider::Codex)
        .expect("worker advertises codex");
    assert_eq!(peer.capabilities, vec![Provider::Claude, Provider::Codex]);
    assert!(boss.find_peer(Provider::Copilot).is_none());

    let access = boss
        .delegate(peer.id, LlmTask::new(Provider::Codex, "access"))
        .await
        .unwrap();
    assert_eq!(
        access, "read-only",
        "peers get read-only unless granted more"
    );
    let mut grant = worker.trusted().get(&boss.id()).unwrap().clone();
    grant.access = Access::WorkspaceWrite;
    worker.allow(grant);
    let access = boss
        .delegate(peer.id, LlmTask::new(Provider::Codex, "access"))
        .await
        .unwrap();
    assert_eq!(
        access, "read-only",
        "the data folder caps every peer at read-only"
    );

    let out = boss
        .delegate(peer.id, LlmTask::new(Provider::Codex, "add tests"))
        .await
        .unwrap();
    assert_eq!(out, "codex did: add tests");
    let received = next_matching(&mut worker_events, |e| {
        matches!(e, Event::TaskReceived { .. })
    })
    .await;
    assert!(
        matches!(received, Event::TaskReceived { from, provider: Provider::Codex, .. } if from == "boss")
    );

    let err = boss
        .delegate(peer.id, LlmTask::new(Provider::Claude, "fail"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "model error");
}

#[tokio::test]
async fn delegation_needs_permission_and_respects_the_hourly_limit() {
    let mut worker = Node::new("worker").serving(vec![Provider::Copilot]);
    let mut chatty = Node::new("chatty");
    let mut limited = Node::new("limited");
    worker.trusts(&chatty, None);
    worker.trusts(&limited, Some(1));
    chatty.trusts(&worker, None);
    limited.trusts(&worker, None);
    let audit_dir = tempfile::tempdir().unwrap();
    let audit_path = audit_dir.path().join("audit.jsonl");
    worker.audit = Some(Arc::new(AuditLog::new(audit_path.clone())));
    let (worker, _we) = worker.start().await;
    let (chatty, _ce) = chatty.start().await;
    let (limited, _le) = limited.start().await;
    chatty.connect(worker.addr()).await.unwrap();
    limited.connect(worker.addr()).await.unwrap();
    wait_for_peers(&worker, 2).await;
    wait_for_peers(&chatty, 1).await;
    wait_for_peers(&limited, 1).await;

    let err = chatty
        .delegate(worker.id(), LlmTask::new(Provider::Copilot, "hi"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not allowed to delegate"), "{err}");

    limited
        .delegate(worker.id(), LlmTask::new(Provider::Copilot, "one"))
        .await
        .unwrap();
    let err = limited
        .delegate(worker.id(), LlmTask::new(Provider::Copilot, "two"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("limit of 1 tasks per hour"),
        "{err}"
    );

    // Every request is audited on the worker: who, outcome and access, no prompt text.
    let entries = AuditLog::read(&audit_path, 10).unwrap();
    let summary: Vec<(&str, Outcome)> = entries
        .iter()
        .map(|e| (e.peer.as_str(), e.outcome))
        .collect();
    assert_eq!(
        summary,
        [
            ("chatty", Outcome::Refused),
            ("limited", Outcome::Done),
            ("limited", Outcome::Refused),
        ]
    );
    assert_eq!(entries[1].access, Some(Access::ReadOnly));
    assert_eq!(entries[1].peer_id, limited.id().to_string());
    assert!(entries[0].error.as_deref().unwrap().contains("not allowed"));
    let raw = std::fs::read_to_string(&audit_path).unwrap();
    assert!(!raw.contains("\"one\"") && !raw.contains("\"two\""));
}

#[tokio::test]
async fn refuses_providers_the_peer_does_not_serve() {
    let mut worker = Node::new("worker").serving(vec![Provider::Claude]);
    let mut plain = Node::new("plain");
    mutual(&mut worker, &mut plain);
    let (worker, _we) = worker.start().await;
    let (plain, _pe) = plain.start().await;
    plain.connect(worker.addr()).await.unwrap();
    wait_for_peers(&plain, 1).await;
    wait_for_peers(&worker, 1).await;

    let err = plain
        .delegate(worker.id(), LlmTask::new(Provider::Copilot, "hi"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not serve copilot"), "{err}");

    // An agent without an executor advertises nothing, so nothing can be delegated to it.
    assert!(plain.capabilities().is_empty());
    let err = worker
        .delegate(plain.id(), LlmTask::new(Provider::Claude, "hi"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not serve claude"), "{err}");
}

#[tokio::test]
async fn delegated_task_fails_when_peer_disconnects() {
    let mut worker = Node::new("worker").serving(vec![Provider::Copilot]);
    let mut boss = Node::new("boss");
    mutual(&mut worker, &mut boss);
    let (worker, mut worker_events) = worker.start().await;
    let (boss, _be) = boss.start().await;
    boss.connect(worker.addr()).await.unwrap();
    wait_for_peers(&boss, 1).await;

    let pending = tokio::spawn({
        let boss = boss.clone();
        let id = worker.id();
        async move {
            boss.delegate(id, LlmTask::new(Provider::Copilot, "hang"))
                .await
        }
    });
    next_matching(&mut worker_events, |e| {
        matches!(e, Event::TaskReceived { .. })
    })
    .await;
    worker.shutdown().await;

    let err = timeout(Duration::from_secs(10), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("disconnected"), "{err}");
}

/// Waits until `count` hanging runs on the worker have been stopped.
async fn wait_stopped(stopped: &AtomicUsize, count: usize) {
    timeout(Duration::from_secs(10), async {
        while stopped.load(Ordering::SeqCst) < count {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "only {} of {count} runs stopped",
            stopped.load(Ordering::SeqCst)
        )
    });
}

#[tokio::test]
async fn cancelling_a_delegated_task_stops_it_on_the_worker() {
    let mut worker = Node::new("worker").serving(vec![Provider::Copilot]);
    let mut boss = Node::new("boss");
    mutual(&mut worker, &mut boss);
    let audit_dir = tempfile::tempdir().unwrap();
    let audit_path = audit_dir.path().join("audit.jsonl");
    worker.audit = Some(Arc::new(AuditLog::new(audit_path.clone())));
    let stopped = worker.stopped.clone();
    let (worker, mut worker_events) = worker.start().await;
    let (boss, _be) = boss.start().await;
    boss.connect(worker.addr()).await.unwrap();
    wait_for_peers(&boss, 1).await;

    // Fill both task slots with hanging tasks, then cancel them from the requester.
    let mut hanging = Vec::new();
    for _ in 0..2 {
        let (boss, id) = (boss.clone(), worker.id());
        hanging.push(tokio::spawn(async move {
            boss.delegate(id, LlmTask::new(Provider::Copilot, "hang"))
                .await
        }));
        next_matching(&mut worker_events, |e| {
            matches!(e, Event::TaskReceived { .. })
        })
        .await;
    }
    for task in &hanging {
        task.abort();
    }
    wait_stopped(&stopped, 2).await;
    for _ in 0..2 {
        let completed = next_matching(&mut worker_events, |e| {
            matches!(e, Event::TaskCompleted { .. })
        })
        .await;
        assert!(
            matches!(completed, Event::TaskCompleted { outcome: Outcome::Cancelled, ref from, .. } if from == "boss"),
            "{completed:?}"
        );
    }

    // The slots are free again: a new task runs right away.
    let out = timeout(
        Duration::from_secs(10),
        boss.delegate(worker.id(), LlmTask::new(Provider::Copilot, "after")),
    )
    .await
    .expect("slots were freed")
    .unwrap();
    assert_eq!(out, "copilot did: after");

    let outcomes: Vec<Outcome> = AuditLog::read(&audit_path, 10)
        .unwrap()
        .iter()
        .map(|e| e.outcome)
        .collect();
    assert_eq!(
        outcomes,
        [Outcome::Cancelled, Outcome::Cancelled, Outcome::Done]
    );
}

#[tokio::test]
async fn worker_cancels_tasks_when_the_requester_disconnects() {
    let mut worker = Node::new("worker").serving(vec![Provider::Copilot]);
    let mut boss = Node::new("boss");
    mutual(&mut worker, &mut boss);
    let stopped = worker.stopped.clone();
    let (worker, mut worker_events) = worker.start().await;
    let (boss, _be) = boss.start().await;
    boss.connect(worker.addr()).await.unwrap();
    wait_for_peers(&boss, 1).await;

    let _pending = tokio::spawn({
        let (boss, id) = (boss.clone(), worker.id());
        async move {
            boss.delegate(id, LlmTask::new(Provider::Copilot, "hang"))
                .await
        }
    });
    next_matching(&mut worker_events, |e| {
        matches!(e, Event::TaskReceived { .. })
    })
    .await;
    boss.shutdown().await;

    wait_stopped(&stopped, 1).await;
    let completed = next_matching(&mut worker_events, |e| {
        matches!(e, Event::TaskCompleted { .. })
    })
    .await;
    assert!(
        matches!(
            completed,
            Event::TaskCompleted {
                outcome: Outcome::Cancelled,
                ..
            }
        ),
        "{completed:?}"
    );
}

#[tokio::test]
async fn peer_tasks_run_in_the_shared_data_folder_by_default() {
    let mut worker = Node::new("worker").serving(vec![Provider::Copilot]);
    let mut boss = Node::new("boss");
    mutual(&mut worker, &mut boss);
    let (worker, _we) = worker.start().await;
    let (boss, _be) = boss.start().await;
    boss.connect(worker.addr()).await.unwrap();
    wait_for_peers(&boss, 1).await;

    let peer = boss.find_peer(Provider::Copilot).unwrap();
    assert_eq!(
        peer.shares,
        [ShareInfo {
            name: DATA_SHARE.into(),
            access: Access::ReadOnly
        }],
        "peers learn folder names and access, never paths"
    );
    let folder = boss
        .delegate(peer.id, LlmTask::new(Provider::Copilot, "where"))
        .await
        .unwrap();
    assert_eq!(folder, data_folder("worker").display().to_string());
    let access = boss
        .delegate(peer.id, LlmTask::new(Provider::Copilot, "access"))
        .await
        .unwrap();
    assert_eq!(access, "read-only");
}

#[tokio::test]
async fn peers_reach_only_folders_shared_with_them() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["code", "secret"] {
        std::fs::create_dir(dir.path().join(name)).unwrap();
    }
    let mut worker = Node::new("worker").serving(vec![Provider::Copilot]);
    worker
        .shares
        .add("code", &dir.path().join("code"), Access::WorkspaceWrite)
        .unwrap();
    worker
        .shares
        .add("secret", &dir.path().join("secret"), Access::Full)
        .unwrap();
    let mut boss = Node::new("boss");
    let mut other = Node::new("other");
    mutual(&mut worker, &mut boss);
    mutual(&mut worker, &mut other);
    // boss may use data and code, with full access requested; other only data.
    let boss_grant = worker
        .trust
        .peers
        .iter_mut()
        .find(|p| p.name == "boss")
        .unwrap();
    boss_grant.access = Access::Full;
    boss_grant.shares = vec![DATA_SHARE.into(), "code".into()];
    let (worker, _we) = worker.start().await;
    let (boss, _be) = boss.start().await;
    let (other, _oe) = other.start().await;
    boss.connect(worker.addr()).await.unwrap();
    other.connect(worker.addr()).await.unwrap();
    wait_for_peers(&boss, 1).await;
    wait_for_peers(&other, 1).await;

    let seen: Vec<(String, Access)> = boss.peers()[0]
        .shares
        .iter()
        .map(|s| (s.name.clone(), s.access))
        .collect();
    assert_eq!(
        seen,
        [
            (DATA_SHARE.to_owned(), Access::ReadOnly),
            ("code".to_owned(), Access::WorkspaceWrite)
        ],
        "the unshared folder isn't even listed; access is capped per folder"
    );

    let in_code = |prompt: &str| LlmTask::new(Provider::Copilot, prompt).in_share("code");
    let folder = boss.delegate(worker.id(), in_code("where")).await.unwrap();
    assert_eq!(
        std::fs::canonicalize(&folder).unwrap(),
        std::fs::canonicalize(dir.path().join("code")).unwrap()
    );
    let access = boss.delegate(worker.id(), in_code("access")).await.unwrap();
    assert_eq!(
        access, "workspace-write",
        "lower of full grant and folder max"
    );

    for (who, share) in [(&boss, "secret"), (&other, "code"), (&boss, "missing")] {
        let task = LlmTask::new(Provider::Copilot, "where").in_share(share);
        let err = who.delegate(worker.id(), task).await.unwrap_err();
        assert!(err.to_string().contains("has not shared a folder"), "{err}");
    }

    // Narrowing a connected peer's grant reaches it right away.
    let mut grant = worker.trusted().get(&boss.id()).unwrap().clone();
    grant.shares = vec![DATA_SHARE.into()];
    worker.allow(grant);
    timeout(Duration::from_secs(10), async {
        while boss.peers()[0].shares.len() != 1 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("boss saw its narrowed folder list");
    let err = boss
        .delegate(worker.id(), in_code("where"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("has not shared a folder"), "{err}");
}
