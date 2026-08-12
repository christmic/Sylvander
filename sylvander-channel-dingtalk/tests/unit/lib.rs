use super::*;
use sylvander_channel::credential::{
    CredentialLeaseBundle, CredentialLeaseError, CredentialLeaseRequest, CredentialLeaseSource,
};

struct StaticCredentials;

#[async_trait]
impl CredentialLeaseSource for StaticCredentials {
    async fn lease(
        &self,
        request: &CredentialLeaseRequest,
    ) -> Result<CredentialLeaseBundle, CredentialLeaseError> {
        CredentialLeaseBundle::new(
            1,
            1,
            1,
            31,
            request
                .slots
                .iter()
                .map(|slot| (slot.clone(), format!("{slot}-value").into_bytes())),
        )
    }
}

#[test]
fn request_limit_is_configurable() {
    let channel = DingTalkChannel::new("bot-a", "agent", Arc::new(StaticCredentials))
        .unwrap()
        .with_request_limit(4096);
    assert_eq!(channel.client.max_message_bytes, 4096);
}

#[tokio::test]
async fn readiness_is_reported_only_by_the_connected_callback() {
    let readiness = sylvander_channel::ChannelReadiness::new();
    let context = Arc::new(ChannelContext::with_services(
        Arc::new(sylvander_channel::InProcessMessageBus::new()),
        Some("bot-a".into()),
        None,
        Some(readiness.clone()),
    ));
    let handler = ChannelMessageHandler {
        ctx: context,
        instance_id: "bot-a".into(),
        agent_id: AgentId::new("agent"),
        replay: Arc::new(ReplayCache::default()),
        client: Client::new("bot-a", Arc::new(StaticCredentials)).unwrap(),
    };

    assert!(!readiness.is_ready());
    handler.on_connected().await;
    assert!(readiness.is_ready());
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
