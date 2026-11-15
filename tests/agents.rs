//! Multi-agent tests over real iroh QUIC endpoints on localhost (`Network::Local`:
//! direct IPs only, no relays or lookup services, so no internet is needed).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use p2pagents::audit::{AuditLog, Outcome};
use p2pagents::{
    Access, Agent, AgentConfig, EndpointId, Event, Executor, LlmTask, Network, Provider, SecretKey,
    TrustStore, TrustedPeer,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

/// Echoes the prompt; "hang" never completes, "fail" errors and "access" returns
/// the access the agent granted the task.
struct FakeExecutor(Vec<Provider>);

#[async_trait]
impl Executor for FakeExecutor {
    fn providers(&self) -> Vec<Provider> {
        self.0.clone()
    }

    async fn execute(&self, task: &LlmTask, grant: Access) -> anyhow::Result<String> {
        match task.prompt.as_str() {
            "hang" => std::future::pending().await,
            "fail" => anyhow::bail!("model error"),
            "access" => Ok(grant.to_string()),
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
}

impl Node {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            key: SecretKey::generate(),
            trust: TrustStore::default(),
            executor: None,
            audit: None,
        }
    }

    fn id(&self) -> EndpointId {
        self.key.public()
    }

    fn serving(mut self, providers: Vec<Provider>) -> Self {
        self.executor = Some(Arc::new(FakeExecutor(providers)));
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
        });
    }

    async fn start(self) -> (Agent, UnboundedReceiver<Event>) {
        let mut config = AgentConfig::new(self.name, self.key);
        config.network = Network::Local;
        config.bind = Some("127.0.0.1:0".parse().unwrap());
        config.trust = self.trust;
        config.executor = self.executor;
        config.audit = self.audit;
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
    assert_eq!(access, "workspace-write");

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
