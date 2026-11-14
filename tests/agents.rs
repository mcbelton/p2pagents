use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use p2pagents::{Agent, Event, Executor, LlmTask, Provider};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

/// Echoes the prompt; a prompt of "hang" never completes.
struct FakeExecutor(Vec<Provider>);

#[async_trait]
impl Executor for FakeExecutor {
    fn providers(&self) -> Vec<Provider> {
        self.0.clone()
    }

    async fn execute(&self, task: &LlmTask) -> anyhow::Result<String> {
        match task.prompt.as_str() {
            "hang" => std::future::pending().await,
            "fail" => anyhow::bail!("model error"),
            prompt => Ok(format!("{} did: {prompt}", task.provider)),
        }
    }
}

async fn serving_agent(name: &str, providers: Vec<Provider>) -> (Agent, UnboundedReceiver<Event>) {
    Agent::bind_with_executor(name, "127.0.0.1:0", Arc::new(FakeExecutor(providers)))
        .await
        .unwrap()
}

async fn next_matching(
    events: &mut UnboundedReceiver<Event>,
    pred: impl Fn(&Event) -> bool,
) -> Event {
    timeout(Duration::from_secs(5), async {
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
    timeout(Duration::from_secs(5), async {
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
    let (a, _a_events) = Agent::bind("a", "127.0.0.1:0").await.unwrap();
    let (b, _b_events) = Agent::bind("b", "127.0.0.1:0").await.unwrap();
    let (c, mut c_events) = Agent::bind("c", "127.0.0.1:0").await.unwrap();

    b.connect(a.listen_addr()).await.unwrap();
    wait_for_peers(&a, 1).await;
    // c only knows a; it learns about b from a's peer list.
    c.connect(a.listen_addr()).await.unwrap();

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
async fn simultaneous_dials_converge_on_one_connection() {
    let (a, _a_events) = Agent::bind("a", "127.0.0.1:0").await.unwrap();
    let (b, mut b_events) = Agent::bind("b", "127.0.0.1:0").await.unwrap();

    let (ra, rb) = tokio::join!(a.connect(b.listen_addr()), b.connect(a.listen_addr()));
    ra.unwrap();
    rb.unwrap();
    wait_for_peers(&a, 1).await;
    wait_for_peers(&b, 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

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
    let (a, mut a_events) = Agent::bind("a", "127.0.0.1:0").await.unwrap();
    let (b, _b_events) = Agent::bind("b", "127.0.0.1:0").await.unwrap();

    b.connect(a.listen_addr()).await.unwrap();
    next_matching(&mut a_events, |e| matches!(e, Event::PeerConnected { .. })).await;

    b.shutdown();
    let event = next_matching(&mut a_events, |e| {
        matches!(e, Event::PeerDisconnected { .. })
    })
    .await;
    assert_eq!(
        event,
        Event::PeerDisconnected {
            name: "b".into(),
            addr: b.listen_addr()
        }
    );
    assert!(a.peers().is_empty());
    assert!(b.connect(a.listen_addr()).await.is_err());
}

#[tokio::test]
async fn delegates_tasks_to_a_peer_that_serves_the_provider() {
    let (worker, mut worker_events) =
        serving_agent("worker", vec![Provider::Claude, Provider::Codex]).await;
    let (boss, _boss_events) = Agent::bind("boss", "127.0.0.1:0").await.unwrap();
    boss.connect(worker.listen_addr()).await.unwrap();
    wait_for_peers(&boss, 1).await;

    let peer = boss
        .find_peer(Provider::Codex)
        .expect("worker advertises codex");
    assert_eq!(peer.capabilities, vec![Provider::Claude, Provider::Codex]);
    assert!(boss.find_peer(Provider::Copilot).is_none());

    let out = boss
        .delegate(peer.addr, LlmTask::new(Provider::Codex, "add tests"))
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
    let done = next_matching(&mut worker_events, |e| {
        matches!(e, Event::TaskCompleted { .. })
    })
    .await;
    assert!(matches!(done, Event::TaskCompleted { ok: true, .. }));

    let err = boss
        .delegate(peer.addr, LlmTask::new(Provider::Claude, "fail"))
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "model error");
}

#[tokio::test]
async fn refuses_providers_the_peer_does_not_serve() {
    let (worker, _worker_events) = serving_agent("worker", vec![Provider::Claude]).await;
    let (plain, _plain_events) = Agent::bind("plain", "127.0.0.1:0").await.unwrap();
    plain.connect(worker.listen_addr()).await.unwrap();
    wait_for_peers(&plain, 1).await;
    wait_for_peers(&worker, 1).await;

    let err = plain
        .delegate(worker.listen_addr(), LlmTask::new(Provider::Copilot, "hi"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not serve copilot"), "{err}");

    // An agent without an executor rejects tasks even if asked directly.
    assert!(plain.capabilities().is_empty());
    let err = worker
        .delegate(plain.listen_addr(), LlmTask::new(Provider::Claude, "hi"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("does not serve claude"), "{err}");
}

#[tokio::test]
async fn delegated_task_fails_when_peer_disconnects() {
    let (worker, mut worker_events) = serving_agent("worker", vec![Provider::Copilot]).await;
    let (boss, _boss_events) = Agent::bind("boss", "127.0.0.1:0").await.unwrap();
    boss.connect(worker.listen_addr()).await.unwrap();
    wait_for_peers(&boss, 1).await;

    let pending = tokio::spawn({
        let boss = boss.clone();
        let addr = worker.listen_addr();
        async move {
            boss.delegate(addr, LlmTask::new(Provider::Copilot, "hang"))
                .await
        }
    });
    next_matching(&mut worker_events, |e| {
        matches!(e, Event::TaskReceived { .. })
    })
    .await;
    worker.shutdown();

    let err = timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(err.to_string().contains("disconnected"), "{err}");
}
