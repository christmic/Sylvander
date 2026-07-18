//! Telegram bot channel — webhook incoming, sendMessage outgoing.
//!
//! # Setup
//!
//! ```text
//! export TELEGRAM_BOT_TOKEN=...
//! # webhook URL: https://your-host/telegram/webhook
//! curl -X POST https://api.telegram.org/bot${TOKEN}/setWebhook \
//!   -d "url=https://your-host/telegram/webhook"
//! ```

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use sylvander_agent::bus::{MessageKind, StreamEvent, SubscriptionFilter};
use sylvander_agent::session_store::SessionStore;
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{
    Channel, ChannelContext, ExternalChatRequest, parse_external_control, submit_external_chat,
};
use sylvander_protocol::{
    AuthenticatedPrincipal, AuthenticationFailure, AuthenticationMethod, BoundaryContext,
    BoundaryErrorCode,
};

// ===========================================================================
// Telegram types
// ===========================================================================

#[derive(Debug, Deserialize)]
pub struct Update {
    #[serde(rename = "update_id")]
    pub update_id: i64,
    pub message: Option<Message>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    #[serde(rename = "message_id")]
    pub message_id: i64,
    pub from: Option<User>,
    pub chat: Chat,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(rename = "first_name")]
    pub first_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

#[derive(Debug, Serialize)]
struct SendMessage {
    chat_id: i64,
    text: String,
}

// ===========================================================================
// Channel
// ===========================================================================

pub struct TelegramChannel {
    token: String,
    webhook_addr: SocketAddr,
    agent_id: AgentId,
    /// `chat_id` → bot `message_id` (for `editMessageText` during streaming)
    last_bot_msg: Arc<RwLock<HashMap<i64, i32>>>,
    http: reqwest::Client,
    api_base_url: String,
    webhook_secret: Option<String>,
    instance_id: String,
    max_request_bytes: usize,
}

impl TelegramChannel {
    pub fn new(
        token: impl Into<String>,
        webhook_addr: SocketAddr,
        agent_id: impl Into<AgentId>,
    ) -> Self {
        Self {
            token: token.into(),
            webhook_addr,
            agent_id: agent_id.into(),
            last_bot_msg: Arc::new(RwLock::new(HashMap::new())),
            http: reqwest::Client::new(),
            api_base_url: "https://api.telegram.org".into(),
            webhook_secret: None,
            instance_id: "telegram".into(),
            max_request_bytes: 1024 * 1024,
        }
    }

    /// Require Telegram's secret-token header on every webhook request.
    #[must_use]
    pub fn with_webhook_secret(mut self, secret: impl Into<String>) -> Self {
        self.webhook_secret = Some(secret.into());
        self
    }

    /// Identify this configured bot instance for session and principal isolation.
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

    fn api(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.api_base_url, self.token, method)
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &'static str {
        "telegram"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);

        // Outgoing loop: subscribe to all events → sendMessage via bot API
        let ch = self.clone();
        let ctx_out = ctx.clone();
        let outgoing = tokio::spawn(async move { run_outgoing(ch.clone(), ctx_out).await });

        // HTTP server for incoming webhooks
        let state = Arc::new(AppState {
            sessions: ctx.sessions.clone(),
            ctx,
            channel: self.clone(),
            agent_id: self.agent_id.clone(),
            webhook_secret: self.webhook_secret.clone(),
            instance_id: self.instance_id.clone(),
            replay: ReplayCache::default(),
        });

        let app = Router::new()
            .route("/telegram/webhook", post(handle_webhook))
            .layer(DefaultBodyLimit::max(self.max_request_bytes))
            .with_state(state.clone());

        let listener = match tokio::net::TcpListener::bind(self.webhook_addr).await {
            Ok(listener) => listener,
            Err(error) => {
                warn!(%error, addr = %self.webhook_addr, "telegram channel bind failed");
                outgoing.abort();
                let _ = outgoing.await;
                return;
            }
        };
        info!(addr = %self.webhook_addr, "telegram channel listening");
        state.ctx.mark_ready();
        let shutdown = state.ctx.clone();
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.shutdown_requested().await })
            .await
        {
            warn!(%error, "telegram channel server failed");
        }
        outgoing.abort();
        let _ = outgoing.await;
    }
}

// Clone for spawning outgoing task
impl Clone for TelegramChannel {
    fn clone(&self) -> Self {
        Self {
            token: self.token.clone(),
            webhook_addr: self.webhook_addr,
            agent_id: self.agent_id.clone(),
            last_bot_msg: self.last_bot_msg.clone(),
            http: self.http.clone(),
            api_base_url: self.api_base_url.clone(),
            webhook_secret: self.webhook_secret.clone(),
            instance_id: self.instance_id.clone(),
            max_request_bytes: self.max_request_bytes,
        }
    }
}

// ===========================================================================
// Incoming: webhook → bus
// ===========================================================================

struct AppState {
    ctx: Arc<ChannelContext>,
    channel: Arc<TelegramChannel>,
    agent_id: AgentId,
    sessions: Arc<dyn SessionStore>,
    webhook_secret: Option<String>,
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

async fn handle_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(update): Json<Update>,
) -> Result<&'static str, StatusCode> {
    if !valid_webhook_secret(&headers, state.webhook_secret.as_deref()) {
        warn!("telegram: rejected webhook with invalid secret token");
        return Err(reject_webhook_authentication(&state).await);
    }
    let Some(msg) = update.message else {
        return Ok("ok");
    };
    let Some(text) = msg.text else {
        return Ok("ok");
    };
    let update_id = update.update_id.to_string();
    if !state.replay.claim(&update_id).await {
        info!(%update_id, "telegram: ignored duplicate update");
        return Ok("ok");
    }

    let chat_id = msg.chat.id;
    let chat_id_str = chat_id.to_string();
    let principal_id = platform_principal_id(&state.instance_id, &chat_id_str);

    // Find or create session
    let existing = find_by_chat_id(&state.sessions, &state.instance_id, &chat_id_str).await;
    let boundary = BoundaryContext::authenticated(
        AuthenticatedPrincipal::user(principal_id.clone(), AuthenticationMethod::PlatformIdentity),
        &state.instance_id,
        "telegram",
        format!("telegram-update-{}", update.update_id),
    );
    if let Some(control) = parse_external_control(&text, existing.as_ref()) {
        let response = match control {
            Ok(control) => match state.ctx.submit_control(&boundary, control).await {
                Ok(()) => "control accepted".to_string(),
                Err(error) => {
                    warn!(code = ?error.code, request_id = %error.request_id, "telegram: control denied");
                    "control rejected".to_string()
                }
            },
            Err(message) => message.to_string(),
        };
        send_message(&state.channel, chat_id, &response).await;
        return Ok("ok");
    }
    let external_meta = BTreeMap::from([
        ("channel_instance_id".into(), state.instance_id.clone()),
        ("chat_id".into(), chat_id_str.clone()),
    ]);
    let submitted = match submit_external_chat(
        &state.ctx,
        &boundary,
        ExternalChatRequest {
            existing_session: existing,
            agent_id: state.agent_id.clone(),
            label: format!("telegram-{chat_id}"),
            overrides: sylvander_protocol::SessionConfigOverrides::default(),
            text: text.clone(),
            attachments: Vec::new(),
            external_meta,
        },
    )
    .await
    {
        Ok(submitted) => submitted,
        Err(error) => {
            warn!(code = ?error.code, request_id = %error.request_id, "telegram: message denied");
            return Ok("denied");
        }
    };
    drop(submitted.events);
    let sender_name = msg
        .from
        .as_ref()
        .map_or_else(|| "user".into(), |user| user.first_name.clone());

    info!(%chat_id, sender = %sender_name, text, "telegram: message received");
    Ok("ok")
}

async fn reject_webhook_authentication(state: &AppState) -> StatusCode {
    let boundary = BoundaryContext::unauthenticated(
        &state.instance_id,
        "telegram",
        uuid::Uuid::new_v4().to_string(),
    );
    let Some(ui) = &state.ctx.ui else {
        return StatusCode::UNAUTHORIZED;
    };
    let error = ui
        .reject_authentication(
            &boundary,
            AuthenticationFailure::new(AuthenticationMethod::WebhookSignature),
        )
        .await;
    if error.code == BoundaryErrorCode::RateLimited {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::UNAUTHORIZED
    }
}

fn valid_webhook_secret(headers: &HeaderMap, expected: Option<&str>) -> bool {
    let Some(expected) = expected.filter(|secret| !secret.is_empty()) else {
        return false;
    };
    headers
        .get("x-telegram-bot-api-secret-token")
        .and_then(|value| value.to_str().ok())
        == Some(expected)
}

fn platform_principal_id(instance_id: &str, chat_id: &str) -> String {
    format!("telegram:{instance_id}:{chat_id}")
}

async fn find_by_chat_id(
    store: &Arc<dyn SessionStore>,
    instance_id: &str,
    chat_id: &str,
) -> Option<SessionId> {
    let list = store.list_persistent().await.ok()?;
    for s in &list {
        if s.external_meta
            .get("channel_instance_id")
            .and_then(|v| v.as_str())
            == Some(instance_id)
            && s.external_meta.get("chat_id").and_then(|v| v.as_str()) == Some(chat_id)
        {
            return Some(s.id.clone());
        }
    }
    None
}

// ===========================================================================
// Outgoing: bus → sendMessage
// ===========================================================================

async fn run_outgoing(ch: Arc<TelegramChannel>, ctx: Arc<ChannelContext>) {
    let mut rx = match ctx
        .subscribe(SubscriptionFilter::for_agent(ch.agent_id.clone()))
        .await
    {
        Ok(receiver) => receiver,
        Err(error) => {
            warn!(%error, "telegram: outgoing subscribe failed");
            return;
        }
    };

    while let Some(msg) = rx.recv().await {
        let MessageKind::Stream(ref ev) = msg.kind else {
            continue;
        };

        let Some(chat_id) = get_chat_id(&ctx.sessions, &msg.session_id, &ch.instance_id).await
        else {
            continue;
        };

        let text = match ev {
            StreamEvent::TextDelta { delta } => delta.clone(),
            StreamEvent::Done { text } => {
                send_message(&ch, chat_id, text).await;
                continue;
            }
            StreamEvent::ToolCall { tool_name, .. } => format!("🔧 calling {tool_name}"),
            StreamEvent::ToolResult {
                tool_name,
                output,
                is_error,
                ..
            } => {
                let icon = if *is_error { "❌" } else { "✅" };
                let summary = if output.chars().count() > 200 {
                    format!("{}...", output.chars().take(200).collect::<String>())
                } else {
                    output.clone()
                };
                format!("{icon} {tool_name}: {summary}")
            }
            StreamEvent::ToolApprovalRequired {
                batch_id, tools, ..
            } => {
                let list: Vec<String> =
                    tools.iter().map(|t| format!("- {}", t.tool_name)).collect();
                format!(
                    "⚠️ approval needed:\n{}\n/approve {batch_id}\n/deny {batch_id} [reason]",
                    list.join("\n")
                )
            }
            StreamEvent::AskUser {
                call_id,
                question,
                options,
                ..
            } => {
                let options = options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| format!("{}. {option}", index + 1))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{question}\n{options}\n/answer {call_id} <answer>")
            }
            StreamEvent::IterationStart { iteration } => {
                format!("💭 thinking... (round {iteration})")
            }
            _ => continue,
        };

        send_message(&ch, chat_id, &text).await;
    }
}

async fn get_chat_id(
    store: &Arc<dyn SessionStore>,
    sid: &SessionId,
    instance_id: &str,
) -> Option<i64> {
    let session = store.get(sid).await.ok()??;
    if session
        .external_meta
        .get("channel_instance_id")
        .and_then(|value| value.as_str())
        != Some(instance_id)
    {
        return None;
    }
    let v = session.external_meta.get("chat_id")?.as_str()?;
    v.parse().ok()
}

async fn send_message(ch: &TelegramChannel, chat_id: i64, text: &str) {
    // Telegram limit: 4096 chars per message
    for chunk in split_message(text, 4096) {
        let body = SendMessage {
            chat_id,
            text: chunk.to_string(),
        };
        let mut delivered = false;
        for attempt in 0..3_u32 {
            match ch.http.post(ch.api("sendMessage")).json(&body).send().await {
                Ok(response) if response.status().is_success() => {
                    delivered = true;
                    break;
                }
                Ok(response)
                    if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS
                        && !response.status().is_server_error() =>
                {
                    warn!(status = %response.status(), "telegram: delivery rejected");
                    return;
                }
                Ok(response) => {
                    warn!(status = %response.status(), attempt, "telegram: delivery retryable");
                }
                Err(error) => {
                    warn!(%error, attempt, "telegram: delivery transport failed");
                }
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(100 * u64::from(attempt + 1))).await;
            }
        }
        if !delivered {
            warn!("telegram: delivery retry budget exhausted");
            return;
        }
    }
}

fn split_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.chars().count() <= max_len {
        return vec![text];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = text[start..]
            .char_indices()
            .nth(max_len)
            .map_or(text.len(), |(index, _)| start + index);
        chunks.push(&text[start..end]);
        start = end;
    }
    chunks
}

fn _unused_json(_v: JsonValue) {}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
