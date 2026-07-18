use super::*;
use sylvander_agent::session::SessionMetadata;
use sylvander_agent::session_store::SqliteSessionStore;
use sylvander_agent::session_store::{SessionLifetime, StoredSession};

#[test]
fn request_limit_is_configurable() {
    let channel = DingTalkChannel::new("key", "secret").with_request_limit(4096);
    assert_eq!(channel.client.max_message_bytes, 4096);
}

#[tokio::test]
async fn conversation_lookup_requires_instance_and_sender() {
    let store: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    let session_id = SessionId::new("session-a");
    let stored = StoredSession::new(
        session_id.clone(),
        "test",
        SessionLifetime::Persistent,
        SessionMetadata {
            workspace: "/tmp".into(),
            name: "test".into(),
            user_id: "dingtalk:bot-a:user-a".into(),
        },
        vec![AgentId::new("agent-a")],
    )
    .with_external_meta("channel_instance_id", "bot-a")
    .with_external_meta("conversation_id", "conversation-a")
    .with_external_meta("sender_staff_id", "user-a")
    .with_external_meta("session_webhook", "https://example.invalid/reply");
    store.save(&stored).await.unwrap();

    assert_eq!(
        find_by_conversation_id(&store, "bot-a", "conversation-a", "user-a").await,
        Some(session_id.clone())
    );
    assert!(
        find_by_conversation_id(&store, "bot-b", "conversation-a", "user-a")
            .await
            .is_none()
    );
    assert!(
        find_by_conversation_id(&store, "bot-a", "conversation-a", "user-b")
            .await
            .is_none()
    );
    assert!(
        get_webhook_url(&store, &session_id, "bot-b")
            .await
            .is_none()
    );
}

#[test]
fn principal_identity_includes_instance_and_sender() {
    assert_eq!(
        platform_principal_id("bot-a", "user-a"),
        "dingtalk:bot-a:user-a"
    );
}

#[tokio::test]
async fn replay_cache_rejects_duplicates_and_is_bounded_and_expiring() {
    let cache = ReplayCache::new(2, Duration::from_mins(1));
    assert!(cache.claim("one").await);
    assert!(!cache.claim("one").await);
    assert!(cache.claim("two").await);
    assert!(cache.claim("three").await);
    assert!(cache.claim("one").await, "oldest entry must be evicted");

    let expiring = ReplayCache::new(2, Duration::ZERO);
    assert!(expiring.claim("one").await);
    assert!(
        expiring.claim("one").await,
        "expired entry must be reusable"
    );
}
