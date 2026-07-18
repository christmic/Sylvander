use super::*;
use crate::bus::{InProcessMessageBus, Recipient};

struct TestRevisionProvider {
    bindings: RwLock<HashMap<SessionId, u64>>,
    runs: HashMap<u64, AgentRun>,
}

#[async_trait::async_trait]
impl RevisionedAgentRunProvider for TestRevisionProvider {
    async fn revision_for_session(
        &self,
        _agent_id: &AgentId,
        session_id: &SessionId,
    ) -> Result<u64, String> {
        self.bindings
            .read()
            .await
            .get(session_id)
            .copied()
            .ok_or_else(|| format!("missing binding for {session_id}"))
    }

    async fn run_for_revision(
        &self,
        _agent_id: &AgentId,
        revision: u64,
    ) -> Result<AgentRun, String> {
        self.runs
            .get(&revision)
            .cloned()
            .ok_or_else(|| format!("missing run for revision {revision}"))
    }
}

fn test_spec(id: &str) -> AgentSpec {
    AgentSpec::builder()
        .id(id)
        .name(format!("Agent {id}"))
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec build")
}

fn test_client() -> AnthropicClient {
    AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .expect("client build")
}

fn test_run(spec: &AgentSpec, bus: Arc<dyn MessageBus>) -> AgentRun {
    AgentRun::builder(spec.clone(), test_client())
        .bus(bus)
        .build()
        .expect("run build")
}

#[tokio::test]
async fn revisioned_run_routes_concurrent_sessions_without_drift() {
    let bus = Arc::new(InProcessMessageBus::new());
    let engine = AgentRunEngine::new(bus.clone());
    let spec = test_spec("revisioned");
    let revision_one = test_run(&spec, bus.clone());
    let revision_two = test_run(&spec, bus.clone());
    let old_session = SessionId::new("old-session");
    let new_session = SessionId::new("new-session");
    let provider = Arc::new(TestRevisionProvider {
        bindings: RwLock::new(HashMap::from([
            (old_session.clone(), 1),
            (new_session.clone(), 2),
        ])),
        runs: HashMap::from([(1, revision_one.clone()), (2, revision_two.clone())]),
    });
    engine
        .spawn_revisioned_run(spec.clone(), 1, revision_one.clone(), provider.clone())
        .await
        .expect("spawn revision router");

    bus.publish(BusMessage::agent_response(
        old_session.clone(),
        spec.id.clone(),
        "self output must not loop back",
    ))
    .await
    .unwrap();
    let mut malformed = BusMessage::user_chat(
        SessionId::new("unknown-session"),
        "attacker",
        "unknown sessions must not stop the router",
    );
    malformed.recipient = Recipient::Agent(spec.id.clone());
    bus.publish(malformed).await.unwrap();

    let metadata = SessionMetadata {
        workspace: "/tmp".into(),
        name: "revision test".into(),
        user_id: "user".into(),
    };
    let (old, new) = tokio::join!(
        engine.attach_session(
            old_session.clone(),
            "old",
            metadata.clone(),
            std::slice::from_ref(&spec.id),
        ),
        engine.attach_session(
            new_session.clone(),
            "new",
            metadata,
            std::slice::from_ref(&spec.id),
        )
    );
    old.expect("attach old revision");
    new.expect("attach new revision");

    tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
        loop {
            if revision_one.get_session(&old_session).await.is_some()
                && revision_two.get_session(&new_session).await.is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both revisions receive their bound sessions");
    assert!(revision_one.get_session(&new_session).await.is_none());
    assert!(revision_two.get_session(&old_session).await.is_none());

    // Changing what a hypothetical new session would bind to cannot
    // mutate the already persisted old-session binding.
    provider
        .bindings
        .write()
        .await
        .insert(SessionId::new("future-session"), 2);
    assert_eq!(
        provider
            .revision_for_session(&spec.id, &old_session)
            .await
            .unwrap(),
        1
    );
    engine.despawn(&spec.id).await.expect("clean shutdown");
    assert!(engine.list_agents().await.is_empty());
}

#[tokio::test]
async fn spawn_and_despawn() {
    let bus = Arc::new(InProcessMessageBus::new());
    let engine = AgentRunEngine::new(bus);

    let handle = engine
        .spawn(test_spec("agent-1"), test_client())
        .await
        .expect("spawn");

    assert_eq!(handle.id, AgentId::new("agent-1"));
    assert_eq!(handle.status, AgentStatus::Starting);

    // Despawn via bus
    engine.despawn(&handle.id).await.expect("despawn");

    // Should be removed
    assert!(engine.get_agent(&AgentId::new("agent-1")).await.is_none());
    assert!(
        tokio::time::timeout(
            tokio::time::Duration::from_millis(20),
            engine.wait_for_agent_exit(),
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn unexpected_agent_exit_is_reported_and_can_be_reaped() {
    let bus = Arc::new(InProcessMessageBus::new());
    let engine = AgentRunEngine::new(bus.clone());
    let agent_id = AgentId::new("agent-1");
    engine
        .spawn(test_spec("agent-1"), test_client())
        .await
        .expect("spawn");

    bus.publish(BusMessage::system_stop(agent_id.clone()))
        .await
        .expect("stop outside engine lifecycle");
    let exited = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        engine.wait_for_agent_exit(),
    )
    .await
    .expect("exit signal")
    .expect("agent id");

    assert_eq!(exited, agent_id);
    engine.despawn(&agent_id).await.expect("reap exited Agent");
    assert!(engine.get_agent(&agent_id).await.is_none());
}

#[tokio::test]
async fn duplicate_spawn_is_error() {
    let bus = Arc::new(InProcessMessageBus::new());
    let engine = AgentRunEngine::new(bus);

    engine
        .spawn(test_spec("dup-agent"), test_client())
        .await
        .expect("first spawn");

    let err = engine
        .spawn(test_spec("dup-agent"), test_client())
        .await
        .unwrap_err();

    assert!(matches!(err, EngineError::AlreadySpawned(_)));
}

#[tokio::test]
async fn concurrent_duplicate_spawn_starts_exactly_one_agent() {
    let bus = Arc::new(InProcessMessageBus::new());
    let engine = Arc::new(AgentRunEngine::new(bus));
    let first = engine.spawn(test_spec("same-agent"), test_client());
    let second = engine.spawn(test_spec("same-agent"), test_client());

    let (first, second) = tokio::join!(first, second);
    assert_ne!(first.is_ok(), second.is_ok());
    assert_eq!(engine.list_agents().await.len(), 1);
    engine
        .despawn(&AgentId::new("same-agent"))
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn create_session_notifies_agents() {
    let bus = Arc::new(InProcessMessageBus::new());
    let engine = AgentRunEngine::new(bus);

    // Spawn agent and drain its status updates so inbox is clean
    let _handle = engine
        .spawn(test_spec("agent-1"), test_client())
        .await
        .expect("spawn");

    // Give the agent a moment to start and publish status
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Now subscribe to the agent's inbox to observe JoinSession
    let mut observer = engine
        .bus()
        .subscribe(crate::bus::SubscriptionFilter::for_agent(AgentId::new(
            "agent-1",
        )))
        .await
        .expect("subscribe");

    // Drain any pending messages (status updates)
    while observer.try_recv().is_ok() {}

    let sid = engine
        .create_session(
            "test-session",
            SessionMetadata {
                workspace: "/tmp".into(),
                name: "test".into(),
                user_id: "user-1".into(),
            },
            &[AgentId::new("agent-1")],
        )
        .await
        .expect("create_session");

    // Agent should receive JoinSession
    let msg = tokio::time::timeout(tokio::time::Duration::from_secs(2), observer.recv())
        .await
        .expect("timeout")
        .expect("should receive JoinSession");

    assert!(matches!(
        msg.kind,
        MessageKind::System(SystemMessage::JoinSession { .. })
    ));

    let meta = engine.get_session(&sid).await.expect("get_session");
    assert_eq!(meta.name, "test-session");

    // Clean up
    engine.despawn(&AgentId::new("agent-1")).await.ok();
}

#[tokio::test]
async fn attach_session_preserves_durable_identity() {
    let bus = Arc::new(InProcessMessageBus::new());
    let engine = AgentRunEngine::new(bus);
    engine
        .spawn(test_spec("agent-1"), test_client())
        .await
        .expect("spawn");

    let durable_id = SessionId::new("durable-session-42");
    engine
        .attach_session(
            durable_id.clone(),
            "restored",
            SessionMetadata {
                workspace: "/tmp/project".into(),
                name: "restored".into(),
                user_id: "user-1".into(),
            },
            &[AgentId::new("agent-1")],
        )
        .await
        .expect("attach session");

    let restored = engine.get_session(&durable_id).await.expect("session");
    assert_eq!(restored.id, durable_id);
    assert_eq!(restored.name, "restored");

    let duplicate = engine
        .attach_session(
            durable_id.clone(),
            "duplicate",
            SessionMetadata {
                workspace: "/tmp/project".into(),
                name: "duplicate".into(),
                user_id: "user-1".into(),
            },
            &[AgentId::new("agent-1")],
        )
        .await;
    assert!(matches!(
        duplicate,
        Err(EngineError::SessionAlreadyAttached(id)) if id == durable_id
    ));

    engine.despawn(&AgentId::new("agent-1")).await.ok();
}

#[tokio::test]
async fn send_message_to_unknown_session_is_error() {
    let bus = Arc::new(InProcessMessageBus::new());
    let engine = AgentRunEngine::new(bus);

    let err = engine
        .send_message(SessionId::new("nonexistent"), Recipient::Broadcast, "hello")
        .await
        .unwrap_err();

    assert!(matches!(err, EngineError::UnknownSession(_)));
}

#[tokio::test]
async fn list_agents_and_sessions() {
    let bus = Arc::new(InProcessMessageBus::new());
    let engine = AgentRunEngine::new(bus);

    engine
        .spawn(test_spec("agent-a"), test_client())
        .await
        .expect("spawn a");
    engine
        .spawn(test_spec("agent-b"), test_client())
        .await
        .expect("spawn b");

    // Let them start
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let agents = engine.list_agents().await;
    assert_eq!(agents.len(), 2);

    engine
        .create_session(
            "multi-agent",
            SessionMetadata {
                workspace: "/tmp".into(),
                name: "multi".into(),
                user_id: "user-1".into(),
            },
            &[AgentId::new("agent-a"), AgentId::new("agent-b")],
        )
        .await
        .expect("create_session");

    assert_eq!(engine.list_sessions().await.len(), 1);

    // Clean up
    engine.despawn(&AgentId::new("agent-a")).await.ok();
    engine.despawn(&AgentId::new("agent-b")).await.ok();
}
