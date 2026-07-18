use super::*;
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, post},
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex as StdMutex, RwLock};
use sylvander_agent::bus::InProcessMessageBus;
use sylvander_agent::session_store::SqliteSessionStore;
use sylvander_channel::UiService;
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
        let values = request
            .slots
            .iter()
            .map(|slot| {
                let value = match slot.as_str() {
                    CALLBACK_TOKEN_SLOT => "token",
                    ENCODING_AES_KEY_SLOT => "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    API_SECRET_SLOT => "api-secret",
                    _ => return Err(CredentialLeaseError::MissingSlot),
                };
                Ok((slot.clone(), value.as_bytes().to_vec()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let now = i64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        )
        .unwrap();
        CredentialLeaseBundle::new(1, 1, now, now + 30, values)
    }
}

struct RotatingCredentials {
    generation: AtomicUsize,
    lease_generation: AtomicUsize,
    api_secret: RwLock<String>,
}

impl RotatingCredentials {
    fn new(api_secret: &str) -> Self {
        Self {
            generation: AtomicUsize::new(1),
            lease_generation: AtomicUsize::new(1),
            api_secret: RwLock::new(api_secret.into()),
        }
    }

    fn rotate(&self, api_secret: &str) {
        *self.api_secret.write().unwrap() = api_secret.into();
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl CredentialLeaseSource for RotatingCredentials {
    async fn lease(
        &self,
        request: &CredentialLeaseRequest,
    ) -> Result<CredentialLeaseBundle, CredentialLeaseError> {
        let api_secret = self.api_secret.read().unwrap().clone();
        let values = request
            .slots
            .iter()
            .map(|slot| {
                let value = match slot.as_str() {
                    CALLBACK_TOKEN_SLOT => "token",
                    ENCODING_AES_KEY_SLOT => "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                    API_SECRET_SLOT => api_secret.as_str(),
                    _ => return Err(CredentialLeaseError::MissingSlot),
                };
                Ok((slot.clone(), value.as_bytes().to_vec()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let now = unix_now();
        CredentialLeaseBundle::new(
            self.generation.load(Ordering::SeqCst) as u64,
            self.lease_generation.fetch_add(1, Ordering::SeqCst) as u64,
            now,
            now + 30,
            values,
        )
    }
}

#[derive(Clone, Default)]
struct ApiFixture {
    token_calls: Arc<AtomicUsize>,
    send_calls: Arc<AtomicUsize>,
    sent_tokens: Arc<StdMutex<Vec<String>>>,
    reject_first_token: Arc<std::sync::atomic::AtomicBool>,
}

async fn token_endpoint(
    State(fixture): State<ApiFixture>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Json<serde_json::Value> {
    fixture.token_calls.fetch_add(1, Ordering::SeqCst);
    let secret = query.get("corpsecret").cloned().unwrap_or_default();
    Json(serde_json::json!({
        "errcode": 0,
        "access_token": format!("access-{secret}"),
        "expires_in": 7200
    }))
}

async fn send_endpoint(
    State(fixture): State<ApiFixture>,
    Query(query): Query<BTreeMap<String, String>>,
    Json(body): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    fixture.send_calls.fetch_add(1, Ordering::SeqCst);
    fixture
        .sent_tokens
        .lock()
        .unwrap()
        .push(query.get("access_token").cloned().unwrap_or_default());
    assert_eq!(body["agentid"], 1_000_001);
    assert_eq!(body["msgtype"], "text");
    if fixture.reject_first_token.swap(false, Ordering::SeqCst) {
        Json(serde_json::json!({"errcode": 40014}))
    } else {
        Json(serde_json::json!({"errcode": 0}))
    }
}

async fn api_fixture(fixture: ApiFixture) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/cgi-bin/gettoken", get(token_endpoint))
        .route("/cgi-bin/message/send", post(send_endpoint))
        .with_state(fixture);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), task)
}

fn unix_now() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

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
        "corp".into(),
        "1000001".into(),
        "127.0.0.1:0".parse().unwrap(),
        "agent",
        "app-a",
        Arc::new(StaticCredentials),
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
    let channel = Arc::new(
        WechatChannel::new(
            "corp".into(),
            "1000001".into(),
            "127.0.0.1:0".parse().unwrap(),
            "agent",
            "app-a",
            Arc::new(StaticCredentials),
        )
        .unwrap(),
    );
    let state = AppState {
        ctx: Arc::new(context),
        channel,
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
    assert_eq!(truncate_utf8_bytes("ab中文", 5), "ab中");
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

#[tokio::test]
async fn active_delivery_reuses_token_and_refreshes_after_credential_rotation() {
    let fixture = ApiFixture::default();
    let (base_url, server) = api_fixture(fixture.clone()).await;
    let credentials = Arc::new(RotatingCredentials::new("first"));
    let channel = WechatChannel::new(
        "corp".into(),
        "1000001".into(),
        "127.0.0.1:0".parse().unwrap(),
        "agent",
        "app-a",
        credentials.clone(),
    )
    .unwrap()
    .with_api_base_url(base_url);

    channel.send_text("user-a", "first reply").await.unwrap();
    channel.send_text("user-a", "second reply").await.unwrap();
    assert_eq!(fixture.token_calls.load(Ordering::SeqCst), 1);

    credentials.rotate("second");
    channel.send_text("user-a", "rotated reply").await.unwrap();
    assert_eq!(fixture.token_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.send_calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        *fixture.sent_tokens.lock().unwrap(),
        ["access-first", "access-first", "access-second"]
    );
    server.abort();
}

#[tokio::test]
async fn invalid_access_token_is_refreshed_once_without_exposing_a_delta() {
    let fixture = ApiFixture::default();
    fixture.reject_first_token.store(true, Ordering::SeqCst);
    let (base_url, server) = api_fixture(fixture.clone()).await;
    let channel = WechatChannel::new(
        "corp".into(),
        "1000001".into(),
        "127.0.0.1:0".parse().unwrap(),
        "agent",
        "app-a",
        Arc::new(RotatingCredentials::new("secret")),
    )
    .unwrap()
    .with_api_base_url(base_url);

    channel
        .send_text("user-a", "complete answer")
        .await
        .unwrap();
    assert_eq!(fixture.token_calls.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.send_calls.load(Ordering::SeqCst), 2);
    server.abort();
}
