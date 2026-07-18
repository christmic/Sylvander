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
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};

use sylvander_agent::bus::{MessageKind, StreamEvent, SubscriptionFilter};
use sylvander_agent::session_store::SessionStore;
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{
    Channel, ChannelContext, ExternalChatRequest,
    credential::{CredentialLeaseRequest, CredentialLeaseSource},
    parse_external_control, submit_external_chat,
};
use sylvander_protocol::{
    AuthenticatedPrincipal, AuthenticationFailure, AuthenticationMethod, BoundaryContext,
};

use protocol::{WechatCrypto, parse_message_xml};

const CALLBACK_TOKEN_SLOT: &str = "callback_token";
const ENCODING_AES_KEY_SLOT: &str = "encoding_aes_key";
const API_SECRET_SLOT: &str = "api_secret";

/// Enterprise `WeChat` callback cryptography and XML protocol handling.
pub mod protocol;

// ===========================================================================
// Channel
// ===========================================================================

/// Authenticated encrypted-XML adapter for one `WeChat` enterprise application.
pub struct WechatChannel {
    corp_id: String,
    wechat_agent_id: u64,
    webhook_addr: SocketAddr,
    agent_id: AgentId,
    instance_id: String,
    credentials: Arc<dyn CredentialLeaseSource>,
    callback_lease: CredentialLeaseRequest,
    api_lease: CredentialLeaseRequest,
    http: reqwest::Client,
    api_base_url: String,
    access_token: Mutex<Option<CachedAccessToken>>,
    max_request_bytes: usize,
}

impl WechatChannel {
    /// Construct one enterprise application with renewable credentials.
    ///
    /// `wechat_agent_id` is the numeric application identifier assigned by
    /// `WeChat` Work. `agent_id` is Sylvander's independent Agent identity.
    pub fn new(
        corp_id: String,
        wechat_agent_id: String,
        webhook_addr: SocketAddr,
        agent_id: impl Into<AgentId>,
        instance_id: impl Into<String>,
        credentials: Arc<dyn CredentialLeaseSource>,
    ) -> Result<Self, String> {
        if corp_id.trim().is_empty() || corp_id.trim() != corp_id {
            return Err("wechat corp_id is invalid".into());
        }
        let wechat_agent_id = wechat_agent_id
            .parse::<u64>()
            .map_err(|_| "wechat agent_id must be numeric".to_string())?;
        if wechat_agent_id == 0 {
            return Err("wechat agent_id must be greater than zero".into());
        }
        let instance_id = instance_id.into();
        let callback_lease = CredentialLeaseRequest::new(
            instance_id.clone(),
            [CALLBACK_TOKEN_SLOT, ENCODING_AES_KEY_SLOT],
        )
        .map_err(|error| error.to_string())?;
        let api_lease = CredentialLeaseRequest::new(instance_id.clone(), [API_SECRET_SLOT])
            .map_err(|error| error.to_string())?;
        Ok(Self {
            corp_id,
            wechat_agent_id,
            webhook_addr,
            agent_id: agent_id.into(),
            instance_id,
            credentials,
            callback_lease,
            api_lease,
            http: reqwest::Client::new(),
            api_base_url: "https://qyapi.weixin.qq.com".into(),
            access_token: Mutex::new(None),
            max_request_bytes: 1024 * 1024,
        })
    }

    /// Bound the encoded webhook body before XML extraction and decryption.
    #[must_use]
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }

    async fn callback_crypto(&self) -> Result<WechatCrypto, String> {
        let lease = self
            .credentials
            .lease(&self.callback_lease)
            .await
            .map_err(|error| error.to_string())?;
        WechatCrypto::new(
            lease
                .secret(CALLBACK_TOKEN_SLOT)
                .map_err(|error| error.to_string())?
                .to_owned(),
            lease
                .secret(ENCODING_AES_KEY_SLOT)
                .map_err(|error| error.to_string())?,
            self.corp_id.clone(),
        )
        .map_err(|error| error.to_string())
    }
}

#[async_trait]
impl Channel for WechatChannel {
    fn name(&self) -> &'static str {
        "wechat"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);

        if let Err(error) = self.callback_crypto().await {
            warn!(%error, instance = %self.instance_id, "wechat callback credential preflight failed");
            return;
        }
        if let Err(error) = self.credentials.lease(&self.api_lease).await {
            warn!(%error, instance = %self.instance_id, "wechat API credential preflight failed");
            return;
        }

        // Outgoing loop: bus → active WeChat Work message API.
        let ch = self.clone();
        let ctx_out = ctx.clone();
        let outgoing = tokio::spawn(async move { run_outgoing(ch.clone(), ctx_out).await });

        // HTTP server
        let state = Arc::new(AppState {
            sessions: ctx.sessions.clone(),
            ctx,
            channel: self.clone(),
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

// ===========================================================================
// App state
// ===========================================================================

struct AppState {
    ctx: Arc<ChannelContext>,
    channel: Arc<WechatChannel>,
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
    let crypto = match state.channel.callback_crypto().await {
        Ok(crypto) => crypto,
        Err(error) => {
            warn!(%error, "wechat: callback credential unavailable");
            reject_webhook_authentication(&state).await;
            return String::new();
        }
    };
    // Verify signature then decrypt echostr
    let echostr = q.echostr.unwrap_or_default();
    if !crypto.verify_signature(&q.msg_signature, &q.timestamp, &q.nonce, &echostr) {
        reject_webhook_authentication(&state).await;
        return String::new();
    }
    if let Ok(msg) = crypto.decrypt(&echostr) {
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
    let crypto = match state.channel.callback_crypto().await {
        Ok(crypto) => crypto,
        Err(error) => {
            warn!(%error, "wechat: callback credential unavailable");
            reject_webhook_authentication(&state).await;
            return "success".into();
        }
    };
    let Some(encrypted) = body.encrypt else {
        reject_webhook_authentication(&state).await;
        return "success".into();
    };

    if !crypto.verify_signature(&q.msg_signature, &q.timestamp, &q.nonce, &encrypted) {
        warn!("wechat: signature invalid");
        reject_webhook_authentication(&state).await;
        return "success".into();
    }

    let xml = match crypto.decrypt(&encrypted) {
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
    if let Some(control) = parse_external_control(&msg.content, existing.as_ref()) {
        let response = match control {
            Ok(control) => match state.ctx.submit_control(&boundary, control).await {
                Ok(()) => "control accepted",
                Err(error) => {
                    warn!(code = ?error.code, request_id = %error.request_id, "wechat: control denied");
                    "control rejected"
                }
            },
            Err(message) => message,
        };
        if let Err(error) = state.channel.send_text(&msg.from_user_name, response).await {
            warn!(%error, "wechat: control response delivery failed");
        }
        return "success".into();
    }
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
            StreamEvent::Done { text } => {
                if let Err(error) = ch.send_text(&user_name, text).await {
                    warn!(%error, "wechat: final reply delivery failed");
                }
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

        if let Err(error) = ch.send_text(&user_name, &text).await {
            warn!(%error, "wechat: event delivery failed");
        }
    }
}

fn truncate_chars(value: &str, limit: usize) -> &str {
    value
        .char_indices()
        .nth(limit)
        .map_or(value, |(index, _)| &value[..index])
}

fn truncate_utf8_bytes(value: &str, limit: usize) -> &str {
    if value.len() <= limit {
        return value;
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
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

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    #[serde(default)]
    errcode: i64,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    expires_in: u64,
}

#[derive(Debug, Deserialize)]
struct ApiResponse {
    #[serde(default)]
    errcode: i64,
}

#[derive(Serialize)]
struct SendTextRequest<'a> {
    touser: &'a str,
    msgtype: &'static str,
    agentid: u64,
    text: SendTextContent<'a>,
    safe: u8,
    enable_id_trans: u8,
    enable_duplicate_check: u8,
    duplicate_check_interval: u16,
}

#[derive(Serialize)]
struct SendTextContent<'a> {
    content: &'a str,
}

struct CachedAccessToken {
    credential_generation: u64,
    expires_at: Instant,
    token: SecretText,
}

impl CachedAccessToken {
    fn is_valid(&self, credential_generation: u64) -> bool {
        self.credential_generation == credential_generation && Instant::now() < self.expires_at
    }
}

struct SecretText(Vec<u8>);

impl SecretText {
    fn from_string(value: String) -> Result<Self, WechatApiError> {
        if value.is_empty() {
            return Err(WechatApiError::InvalidResponse);
        }
        Ok(Self(value.into_bytes()))
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("API token originated from a valid Rust String")
    }
}

impl Drop for SecretText {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretText([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
enum WechatApiError {
    #[error("WeChat credential lease is unavailable")]
    CredentialUnavailable,
    #[error("WeChat API transport failed")]
    Transport,
    #[error("WeChat API returned an invalid response")]
    InvalidResponse,
    #[error("WeChat API rejected the operation")]
    Rejected,
}

impl WechatChannel {
    async fn send_text(&self, to_user: &str, content: &str) -> Result<(), WechatApiError> {
        let lease = self
            .credentials
            .lease(&self.api_lease)
            .await
            .map_err(|_| WechatApiError::CredentialUnavailable)?;
        let api_secret = lease
            .secret(API_SECRET_SLOT)
            .map_err(|_| WechatApiError::CredentialUnavailable)?;
        let generation = lease.credential_generation();
        let mut cached = self.access_token.lock().await;
        if !cached
            .as_ref()
            .is_some_and(|token| token.is_valid(generation))
        {
            *cached = None;
            let token = self.fetch_access_token(api_secret, generation).await?;
            *cached = Some(token);
        }

        let content = truncate_utf8_bytes(content, 2_000);
        let mut refreshed_after_rejection = false;
        for attempt in 0..3 {
            let token = cached
                .as_ref()
                .ok_or(WechatApiError::CredentialUnavailable)?;
            match self
                .send_text_once(token.token.as_str(), to_user, content)
                .await
            {
                Ok(()) => return Ok(()),
                Err(SendAttemptError::InvalidToken) if !refreshed_after_rejection => {
                    *cached = None;
                    let token = self.fetch_access_token(api_secret, generation).await?;
                    *cached = Some(token);
                    refreshed_after_rejection = true;
                }
                Err(SendAttemptError::Retryable) if attempt < 2 => {
                    tokio::time::sleep(Duration::from_millis(100 * (attempt + 1))).await;
                }
                Err(SendAttemptError::Retryable) => {
                    return Err(WechatApiError::Transport);
                }
                Err(SendAttemptError::InvalidToken | SendAttemptError::Rejected) => {
                    return Err(WechatApiError::Rejected);
                }
            }
        }
        Err(WechatApiError::Rejected)
    }

    async fn fetch_access_token(
        &self,
        api_secret: &str,
        credential_generation: u64,
    ) -> Result<CachedAccessToken, WechatApiError> {
        let response = self
            .http
            .get(format!("{}/cgi-bin/gettoken", self.api_base_url))
            .query(&[
                ("corpid", self.corp_id.as_str()),
                ("corpsecret", api_secret),
            ])
            .send()
            .await
            .map_err(|_| WechatApiError::Transport)?;
        if !response.status().is_success() {
            return Err(WechatApiError::Transport);
        }
        let payload = response
            .json::<AccessTokenResponse>()
            .await
            .map_err(|_| WechatApiError::InvalidResponse)?;
        if payload.errcode != 0 || payload.expires_in == 0 {
            return Err(WechatApiError::Rejected);
        }
        let lifetime = payload.expires_in.saturating_sub(60).max(1);
        Ok(CachedAccessToken {
            credential_generation,
            expires_at: Instant::now() + Duration::from_secs(lifetime.min(7_200)),
            token: SecretText::from_string(payload.access_token)?,
        })
    }

    async fn send_text_once(
        &self,
        access_token: &str,
        to_user: &str,
        content: &str,
    ) -> Result<(), SendAttemptError> {
        let body = SendTextRequest {
            touser: to_user,
            msgtype: "text",
            agentid: self.wechat_agent_id,
            text: SendTextContent { content },
            safe: 0,
            enable_id_trans: 0,
            enable_duplicate_check: 1,
            duplicate_check_interval: 1_800,
        };
        let response = self
            .http
            .post(format!("{}/cgi-bin/message/send", self.api_base_url))
            .query(&[("access_token", access_token)])
            .json(&body)
            .send()
            .await
            .map_err(|_| SendAttemptError::Retryable)?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
            || response.status().is_server_error()
        {
            return Err(SendAttemptError::Retryable);
        }
        if !response.status().is_success() {
            return Err(SendAttemptError::Rejected);
        }
        let payload = response
            .json::<ApiResponse>()
            .await
            .map_err(|_| SendAttemptError::Rejected)?;
        match payload.errcode {
            0 => Ok(()),
            40_001 | 40_014 | 42_001 => Err(SendAttemptError::InvalidToken),
            _ => Err(SendAttemptError::Rejected),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendAttemptError {
    InvalidToken,
    Retryable,
    Rejected,
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
