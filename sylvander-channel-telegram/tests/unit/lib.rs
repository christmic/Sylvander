use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use sylvander_agent::bus::InProcessMessageBus;
use sylvander_agent::session_store::SqliteSessionStore;
use sylvander_channel::UiService;

impl TelegramChannel {
    fn with_api_base_url(mut self, api_base_url: impl Into<String>) -> Self {
        self.api_base_url = api_base_url.into();
        self
    }
}

struct AuthenticationRecorder(AtomicUsize);

#[async_trait]
impl UiService for AuthenticationRecorder {
    async fn reject_authentication(
        &self,
        boundary: &BoundaryContext,
        failure: AuthenticationFailure,
    ) -> sylvander_protocol::BoundaryError {
        assert_eq!(boundary.transport, "telegram");
        assert_eq!(
            failure.attempted_method,
            AuthenticationMethod::WebhookSignature
        );
        self.0.fetch_add(1, Ordering::Relaxed);
        sylvander_protocol::BoundaryError {
            code: BoundaryErrorCode::RateLimited,
            operation: failure.operation().into(),
            request_id: boundary.request_id.clone(),
            message: "rate limited".into(),
            retry_after_ms: Some(1_000),
        }
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
    let channel = TelegramChannel::new("token", "127.0.0.1:0".parse().unwrap(), "agent")
        .with_request_limit(4096);
    assert_eq!(channel.max_request_bytes, 4096);
}

#[tokio::test]
async fn invalid_secret_reaches_runtime_authentication_boundary() {
    let ui = Arc::new(AuthenticationRecorder(AtomicUsize::new(0)));
    let sessions = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    let mut context = ChannelContext::new(Arc::new(InProcessMessageBus::new()), sessions.clone());
    context.ui = Some(ui.clone());
    let state = AppState {
        ctx: Arc::new(context),
        channel: Arc::new(TelegramChannel::new(
            "token",
            "127.0.0.1:0".parse().unwrap(),
            "agent",
        )),
        agent_id: AgentId::new("agent"),
        sessions,
        webhook_secret: Some("secret".into()),
        instance_id: "bot-a".into(),
        replay: ReplayCache::default(),
    };

    assert_eq!(
        reject_webhook_authentication(&state).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(ui.0.load(Ordering::Relaxed), 1);
}

#[test]
fn webhook_secret_is_required_by_default() {
    let mut headers = HeaderMap::new();
    assert!(!valid_webhook_secret(&headers, None));
    assert!(!valid_webhook_secret(&headers, Some("")));
    assert!(!valid_webhook_secret(&headers, Some("secret")));
    headers.insert("x-telegram-bot-api-secret-token", "secret".parse().unwrap());
    assert!(valid_webhook_secret(&headers, Some("secret")));
    assert!(!valid_webhook_secret(&headers, Some("other")));
}

#[test]
fn message_split_respects_unicode_character_boundaries() {
    assert_eq!(split_message("中文消息", 2), vec!["中文", "消息"]);
}

#[tokio::test]
async fn delivery_retries_retryable_status_and_then_succeeds() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let state = attempts.clone();
    let app = Router::new().route(
        "/bottoken/sendMessage",
        post(move || {
            let state = state.clone();
            async move {
                if state.fetch_add(1, Ordering::SeqCst) == 0 {
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    StatusCode::OK
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let channel = TelegramChannel::new("token", "127.0.0.1:0".parse().unwrap(), "agent")
        .with_api_base_url(format!("http://{address}"));

    send_message(&channel, 42, "hello").await;

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    server.abort();
}

#[test]
fn principal_identity_includes_instance_and_chat() {
    assert_eq!(platform_principal_id("bot-a", "42"), "telegram:bot-a:42");
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
