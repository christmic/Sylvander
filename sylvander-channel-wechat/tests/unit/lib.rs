use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use sylvander_agent::bus::InProcessMessageBus;
use sylvander_agent::session_store::SqliteSessionStore;
use sylvander_channel::UiService;

struct AuthenticationRecorder(AtomicUsize);

#[async_trait]
impl UiService for AuthenticationRecorder {
    async fn reject_authentication(
        &self,
        boundary: &BoundaryContext,
        failure: AuthenticationFailure,
    ) -> sylvander_protocol::BoundaryError {
        assert_eq!(boundary.transport, "wechat");
        assert_eq!(
            failure.attempted_method,
            AuthenticationMethod::WebhookSignature
        );
        self.0.fetch_add(1, Ordering::Relaxed);
        sylvander_protocol::BoundaryError::unauthenticated(boundary, failure.operation())
    }

    async fn authorize_message(
        &self,
        _: &BoundaryContext,
        _: &sylvander_protocol::UiClientMessage,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        unreachable!()
    }
    async fn discover_agents(
        &self,
        _: &BoundaryContext,
    ) -> Result<Vec<sylvander_protocol::AgentDescriptor>, sylvander_protocol::BoundaryError> {
        unreachable!()
    }
    async fn create_session(
        &self,
        _: &BoundaryContext,
        _: sylvander_protocol::SessionCreateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        unreachable!()
    }
    async fn session_config(
        &self,
        _: &BoundaryContext,
        _: &SessionId,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        unreachable!()
    }
    async fn update_session_config(
        &self,
        _: &BoundaryContext,
        _: sylvander_protocol::SessionConfigUpdateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        unreachable!()
    }
    async fn submit_feedback(
        &self,
        _: &BoundaryContext,
        _: sylvander_protocol::RunFeedback,
    ) -> Result<String, sylvander_protocol::BoundaryError> {
        unreachable!()
    }
}

#[test]
fn request_limit_is_configurable() {
    let channel = WechatChannel::new(
        "token".into(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".into(),
        "corp".into(),
        "127.0.0.1:0".parse().unwrap(),
        "agent",
    )
    .unwrap()
    .with_request_limit(4096);
    assert_eq!(channel.max_request_bytes, 4096);
}

#[tokio::test]
async fn invalid_signature_reaches_runtime_authentication_boundary() {
    let ui = Arc::new(AuthenticationRecorder(AtomicUsize::new(0)));
    let sessions = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    let mut context = ChannelContext::new(Arc::new(InProcessMessageBus::new()), sessions.clone());
    context.ui = Some(ui.clone());
    let state = AppState {
        ctx: Arc::new(context),
        crypto: Arc::new(
            WechatCrypto::new(
                "token".into(),
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                "corp".into(),
            )
            .unwrap(),
        ),
        agent_id: AgentId::new("agent"),
        sessions,
        instance_id: "app-a".into(),
        replay: ReplayCache::default(),
    };

    reject_webhook_authentication(&state).await;
    assert_eq!(ui.0.load(Ordering::Relaxed), 1);
}
#[test]
fn tool_output_truncation_is_unicode_safe() {
    assert_eq!(truncate_chars("中文消息", 2), "中文");
}

#[test]
fn principal_identity_includes_instance_and_user() {
    assert_eq!(
        platform_principal_id("app-a", "user-a"),
        "wechat:app-a:user-a"
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
