//! `WeChat` enterprise bot channel — encrypted XML callbacks.

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    routing::get,
};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{info, warn};

use sylvander_agent::bus::{MessageKind, StreamEvent, SubscriptionFilter};
use sylvander_agent::session_store::SessionStore;
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{Channel, ChannelContext, ExternalChatRequest, submit_external_chat};
use sylvander_protocol::{
    AuthenticatedPrincipal, AuthenticationFailure, AuthenticationMethod, BoundaryContext,
};

use protocol::{WechatCrypto, parse_message_xml};

/// Enterprise WeChat callback cryptography and XML protocol handling.
pub mod protocol;

// ===========================================================================
// Channel
// ===========================================================================

pub struct WechatChannel {
    crypto: Arc<WechatCrypto>,
    webhook_addr: SocketAddr,
    agent_id: AgentId,
    instance_id: String,
    max_request_bytes: usize,
}

impl WechatChannel {
    pub fn new(
        token: String,
        encoding_aes_key: String,
        corp_id: String,
        webhook_addr: SocketAddr,
        agent_id: impl Into<AgentId>,
    ) -> Result<Self, String> {
        let crypto = WechatCrypto::new(token, &encoding_aes_key, corp_id)
            .map_err(|e| format!("wechat crypto init: {e}"))?;
        Ok(Self {
            crypto: Arc::new(crypto),
            webhook_addr,
            agent_id: agent_id.into(),
            instance_id: "wechat".into(),
            max_request_bytes: 1024 * 1024,
        })
    }

    /// Identify this configured application for session and principal isolation.
    #[must_use]
    pub fn with_instance_id(mut self, instance_id: impl Into<String>) -> Self {
        self.instance_id = instance_id.into();
        self
    }

    #[must_use]
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }
}

#[async_trait]
impl Channel for WechatChannel {
    fn name(&self) -> &'static str {
        "wechat"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);

        // Outgoing loop: bus → encrypted XML reply
        let ch = self.clone();
        let ctx_out = ctx.clone();
        let outgoing = tokio::spawn(async move { run_outgoing(ch.clone(), ctx_out).await });

        // HTTP server
        let state = Arc::new(AppState {
            sessions: ctx.sessions.clone(),
            ctx,
            crypto: self.crypto.clone(),
            agent_id: self.agent_id.clone(),
            instance_id: self.instance_id.clone(),
            replay: ReplayCache::default(),
        });

        let app = Router::new()
            .route("/wechat/callback", get(handle_verify).post(handle_callback))
            .layer(DefaultBodyLimit::max(self.max_request_bytes))
            .with_state(state.clone());

        let listener = match tokio::net::TcpListener::bind(self.webhook_addr).await {
            Ok(listener) => listener,
            Err(error) => {
                warn!(%error, addr = %self.webhook_addr, "wechat channel bind failed");
                outgoing.abort();
                let _ = outgoing.await;
                return;
            }
        };
        info!(addr = %self.webhook_addr, "wechat channel listening");
        state.ctx.mark_ready();
        let shutdown = state.ctx.clone();
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.shutdown_requested().await })
            .await
        {
            warn!(%error, "wechat channel server failed");
        }
        outgoing.abort();
        let _ = outgoing.await;
    }
}

impl Clone for WechatChannel {
    fn clone(&self) -> Self {
        Self {
            crypto: self.crypto.clone(),
            webhook_addr: self.webhook_addr,
            agent_id: self.agent_id.clone(),
            instance_id: self.instance_id.clone(),
            max_request_bytes: self.max_request_bytes,
        }
    }
}

// ===========================================================================
// App state
// ===========================================================================

struct AppState {
    ctx: Arc<ChannelContext>,
    crypto: Arc<WechatCrypto>,
    agent_id: AgentId,
    sessions: Arc<dyn SessionStore>,
    instance_id: String,
    replay: ReplayCache,
}

struct ReplayCache {
    entries: Mutex<VecDeque<(String, Instant)>>,
    capacity: usize,
    ttl: Duration,
}

impl ReplayCache {
    fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            capacity: capacity.max(1),
            ttl,
        }
    }

    async fn claim(&self, message_id: &str) -> bool {
        let now = Instant::now();
        let mut entries = self.entries.lock().await;
        entries.retain(|(_, seen)| now.saturating_duration_since(*seen) < self.ttl);
        if entries.iter().any(|(existing, _)| existing == message_id) {
            return false;
        }
        while entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back((message_id.into(), now));
        true
    }
}

impl Default for ReplayCache {
    fn default() -> Self {
        Self::new(4096, Duration::from_mins(10))
    }
}

// ===========================================================================
// URL verification (GET)
// ===========================================================================

#[derive(Deserialize)]
struct CallbackQuery {
    msg_signature: String,
    timestamp: String,
    nonce: String,
    echostr: Option<String>,
}

async fn handle_verify(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CallbackQuery>,
) -> String {
    // Verify signature then decrypt echostr
    let echostr = q.echostr.unwrap_or_default();
    if !state
        .crypto
        .verify_signature(&q.msg_signature, &q.timestamp, &q.nonce, &echostr)
    {
        reject_webhook_authentication(&state).await;
        return String::new();
    }
    if let Ok((msg, _)) = state.crypto.decrypt(&echostr) {
        msg
    } else {
        reject_webhook_authentication(&state).await;
        String::new()
    }
}

// ===========================================================================
// Incoming message (POST)
// ===========================================================================

#[derive(Deserialize)]
struct CallbackBody {
    encrypt: Option<String>,
}

async fn handle_callback(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CallbackQuery>,
    Json(body): Json<CallbackBody>,
) -> String {
    let Some(encrypted) = body.encrypt else {
        reject_webhook_authentication(&state).await;
        return "success".into();
    };

    if !state
        .crypto
        .verify_signature(&q.msg_signature, &q.timestamp, &q.nonce, &encrypted)
    {
        warn!("wechat: signature invalid");
        reject_webhook_authentication(&state).await;
        return "success".into();
    }

    let (xml, _corp_id) = match state.crypto.decrypt(&encrypted) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "wechat: decrypt failed");
            reject_webhook_authentication(&state).await;
            return "success".into();
        }
    };

    let msg = match parse_message_xml(&xml) {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, "wechat: xml parse failed");
            return "success".into();
        }
    };
    if msg.msg_id.is_empty() {
        warn!("wechat: ignored message without a stable id");
        return "success".into();
    }
    if !state.replay.claim(&msg.msg_id).await {
        info!(message_id = %msg.msg_id, "wechat: ignored duplicate message");
        return "success".into();
    }

    info!(from = %msg.from_user_name, msg_type = %msg.msg_type, "wechat: message");

    // Session mapping
    let existing = find_by_user(&state.sessions, &state.instance_id, &msg.from_user_name).await;
    let principal_id = platform_principal_id(&state.instance_id, &msg.from_user_name);
    let boundary = BoundaryContext::authenticated(
        AuthenticatedPrincipal::user(principal_id.clone(), AuthenticationMethod::PlatformIdentity),
        &state.instance_id,
        "wechat",
        format!("wechat-message-{}", msg.msg_id),
    );
    let external_meta = BTreeMap::from([
        ("channel_instance_id".into(), state.instance_id.clone()),
        ("from_user_name".into(), msg.from_user_name.clone()),
    ]);
    let submitted = match submit_external_chat(
        &state.ctx,
        &boundary,
        ExternalChatRequest {
            existing_session: existing,
            agent_id: state.agent_id.clone(),
            label: format!("wechat-{}", msg.from_user_name),
            overrides: sylvander_protocol::SessionConfigOverrides::default(),
            text: msg.content.clone(),
            attachments: Vec::new(),
            external_meta,
        },
    )
    .await
    {
        Ok(submitted) => submitted,
        Err(error) => {
            warn!(code = ?error.code, request_id = %error.request_id, "wechat: message denied");
            return "success".into();
        }
    };
    drop(submitted.events);

    "success".into()
}

async fn reject_webhook_authentication(state: &AppState) {
    let boundary = BoundaryContext::unauthenticated(
        &state.instance_id,
        "wechat",
        uuid::Uuid::new_v4().to_string(),
    );
    if let Some(ui) = &state.ctx.ui {
        let _ = ui
            .reject_authentication(
                &boundary,
                AuthenticationFailure::new(AuthenticationMethod::WebhookSignature),
            )
            .await;
    }
}

fn platform_principal_id(instance_id: &str, user_name: &str) -> String {
    format!("wechat:{instance_id}:{user_name}")
}

async fn find_by_user(
    store: &Arc<dyn SessionStore>,
    instance_id: &str,
    user: &str,
) -> Option<SessionId> {
    let list = store.list_persistent().await.ok()?;
    for s in &list {
        if s.external_meta
            .get("channel_instance_id")
            .and_then(|v| v.as_str())
            == Some(instance_id)
            && s.external_meta
                .get("from_user_name")
                .and_then(|v| v.as_str())
                == Some(user)
        {
            return Some(s.id.clone());
        }
    }
    None
}

// ===========================================================================
// Outgoing: bus → encrypt → reply XML
// ===========================================================================

async fn run_outgoing(ch: Arc<WechatChannel>, ctx: Arc<ChannelContext>) {
    let mut rx = match ctx
        .subscribe(SubscriptionFilter::for_agent(ch.agent_id.clone()))
        .await
    {
        Ok(receiver) => receiver,
        Err(error) => {
            warn!(%error, "wechat: outgoing subscribe failed");
            return;
        }
    };

    while let Some(msg) = rx.recv().await {
        let MessageKind::Stream(ref ev) = msg.kind else {
            continue;
        };

        // Find from_user_name for this session
        let Some(user_name) = get_user_name(&ctx.sessions, &msg.session_id, &ch.instance_id).await
        else {
            continue;
        };

        let text = match ev {
            StreamEvent::TextDelta { delta } => delta.clone(),
            StreamEvent::Done { text } => {
                send_reply(&ch, &user_name, text);
                continue;
            }
            StreamEvent::ToolCall { tool_name, .. } => format!("🔧 {tool_name}"),
            StreamEvent::ToolResult {
                tool_name,
                output,
                is_error,
                ..
            } => {
                let icon = if *is_error { "❌" } else { "✅" };
                format!("{icon} {tool_name}: {}", truncate_chars(output, 200))
            }
            StreamEvent::ToolApprovalRequired { tools, .. } => {
                let list: Vec<String> = tools.iter().map(|t| t.tool_name.clone()).collect();
                format!("⚠️ approval: {}", list.join(", "))
            }
            StreamEvent::IterationStart { iteration } => {
                format!("💭 thinking (round {iteration})")
            }
            _ => continue,
        };

        send_reply(&ch, &user_name, &text);
    }
}

fn truncate_chars(value: &str, limit: usize) -> &str {
    value
        .char_indices()
        .nth(limit)
        .map_or(value, |(index, _)| &value[..index])
}

async fn get_user_name(
    store: &Arc<dyn SessionStore>,
    sid: &SessionId,
    instance_id: &str,
) -> Option<String> {
    let session = store.get(sid).await.ok()??;
    if session
        .external_meta
        .get("channel_instance_id")
        .and_then(|value| value.as_str())
        != Some(instance_id)
    {
        return None;
    }
    session
        .external_meta
        .get("from_user_name")
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn send_reply(ch: &WechatChannel, to_user: &str, content: &str) {
    // Build text reply XML
    let now = chrono::Utc::now().timestamp();
    let text_xml = format!(
        "<xml><ToUserName><![CDATA[{to_user}]]></ToUserName>\
         <FromUserName><![CDATA[{}]]></FromUserName>\
         <CreateTime>{now}</CreateTime>\
         <MsgType><![CDATA[text]]></MsgType>\
         <Content><![CDATA[{content}]]></Content></xml>",
        ch.crypto.corp_id
    );

    // Encrypt
    let timestamp = now.to_string();
    let nonce = uuid::Uuid::new_v4().to_string();
    match ch.crypto.encrypt(&text_xml, &timestamp, &nonce) {
        Ok(encrypted_xml) => {
            // POST back to WeChat — but enterprise app callbacks are passive,
            // so we need active reply API. For simplicity, this is a no-op
            // (real implementation would call https://qyapi.weixin.qq.com/cgi-bin/message/send).
            warn!(
                "wechat: active reply not implemented (logged only): {}",
                encrypted_xml
            );
        }
        Err(e) => warn!(error = %e, "wechat: encrypt reply failed"),
    }
}

#[cfg(test)]
mod tests {
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
        ) -> Result<Vec<sylvander_protocol::AgentDescriptor>, sylvander_protocol::BoundaryError>
        {
            unreachable!()
        }
        async fn create_session(
            &self,
            _: &BoundaryContext,
            _: sylvander_protocol::SessionCreateRequest,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
            unreachable!()
        }
        async fn session_config(
            &self,
            _: &BoundaryContext,
            _: &SessionId,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
            unreachable!()
        }
        async fn update_session_config(
            &self,
            _: &BoundaryContext,
            _: sylvander_protocol::SessionConfigUpdateRequest,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
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
        let mut context =
            ChannelContext::new(Arc::new(InProcessMessageBus::new()), sessions.clone());
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
}
