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

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::RwLock;
use tracing::{info, warn};

use sylvander_agent::bus::{BusMessage, MessageKind, StreamEvent, SubscriptionFilter};
use sylvander_agent::session_store::SessionStore;
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{Channel, ChannelContext, authorize_external_chat};
use sylvander_protocol::{AuthenticatedPrincipal, AuthenticationMethod, BoundaryContext};

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
    webhook_secret: Option<String>,
    instance_id: String,
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
            webhook_secret: None,
            instance_id: "telegram".into(),
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

    fn api(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
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
            agent_id: self.agent_id.clone(),
            webhook_secret: self.webhook_secret.clone(),
            instance_id: self.instance_id.clone(),
        });

        let app = Router::new()
            .route("/telegram/webhook", post(handle_webhook))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind(self.webhook_addr)
            .await
            .unwrap();
        info!(addr = %self.webhook_addr, "telegram channel listening");
        state.ctx.mark_ready();
        let shutdown = state.ctx.clone();
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.shutdown_requested().await })
            .await
            .unwrap();
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
            webhook_secret: self.webhook_secret.clone(),
            instance_id: self.instance_id.clone(),
        }
    }
}

// ===========================================================================
// Incoming: webhook → bus
// ===========================================================================

struct AppState {
    ctx: Arc<ChannelContext>,
    agent_id: AgentId,
    sessions: Arc<dyn SessionStore>,
    webhook_secret: Option<String>,
    instance_id: String,
}

async fn handle_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(update): Json<Update>,
) -> Result<&'static str, StatusCode> {
    if !valid_webhook_secret(&headers, state.webhook_secret.as_deref()) {
        warn!("telegram: rejected webhook with invalid secret token");
        return Err(StatusCode::UNAUTHORIZED);
    }
    let Some(msg) = update.message else {
        return Ok("ok");
    };
    let Some(text) = msg.text else {
        return Ok("ok");
    };

    let chat_id = msg.chat.id;
    let chat_id_str = chat_id.to_string();

    // Find or create session
    let existing = find_by_chat_id(&state.sessions, &state.instance_id, &chat_id_str).await;
    let boundary = BoundaryContext::authenticated(
        AuthenticatedPrincipal::user(
            format!("telegram:{}:{chat_id_str}", state.instance_id),
            AuthenticationMethod::PlatformIdentity,
        ),
        &state.instance_id,
        "telegram",
        format!("telegram-update-{}", update.update_id),
    );
    let external_meta = BTreeMap::from([
        ("channel_instance_id".into(), state.instance_id.clone()),
        ("chat_id".into(), chat_id_str.clone()),
    ]);
    let session_id = match authorize_external_chat(
        &state.ctx,
        &boundary,
        existing,
        state.agent_id.clone(),
        format!("telegram-{chat_id}"),
        &text,
        external_meta,
    )
    .await
    {
        Ok(session_id) => session_id,
        Err(error) => {
            warn!(code = ?error.code, request_id = %error.request_id, "telegram: message denied");
            return Ok("denied");
        }
    };
    let sender_name = msg
        .from
        .as_ref()
        .map_or_else(|| "user".into(), |user| user.first_name.clone());

    if let Ok(Some(session)) = state.sessions.get(&session_id).await {
        let _ = state
            .ctx
            .bus
            .publish(BusMessage::system_join_session(
                state.agent_id.clone(),
                session_id.clone(),
                session.metadata,
            ))
            .await;
    }

    let bus_msg = BusMessage::user_chat(session_id.clone(), &sender_name, &text);
    if let Err(e) = state.ctx.bus.publish(bus_msg).await {
        warn!(error = %e, "telegram: bus publish failed");
        return Ok("error");
    }

    info!(%chat_id, sender = %sender_name, text, "telegram: message received");
    Ok("ok")
}

fn valid_webhook_secret(headers: &HeaderMap, expected: Option<&str>) -> bool {
    expected.is_none_or(|expected| {
        headers
            .get("x-telegram-bot-api-secret-token")
            .and_then(|value| value.to_str().ok())
            == Some(expected)
    })
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
    let mut rx = ctx
        .bus
        .subscribe(SubscriptionFilter::all())
        .await
        .expect("subscribe");

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
                let summary = if output.len() > 200 {
                    format!("{}...", &output[..200])
                } else {
                    output.clone()
                };
                format!("{icon} {tool_name}: {summary}")
            }
            StreamEvent::ToolApprovalRequired { tools, .. } => {
                let list: Vec<String> =
                    tools.iter().map(|t| format!("- {}", t.tool_name)).collect();
                format!("⚠️ approval needed:\n{}", list.join("\n"))
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
        let _ = ch.http.post(ch.api("sendMessage")).json(&body).send().await;
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
mod tests {
    use super::*;

    #[test]
    fn webhook_secret_is_required_when_configured() {
        let mut headers = HeaderMap::new();
        assert!(valid_webhook_secret(&headers, None));
        assert!(!valid_webhook_secret(&headers, Some("secret")));
        headers.insert("x-telegram-bot-api-secret-token", "secret".parse().unwrap());
        assert!(valid_webhook_secret(&headers, Some("secret")));
        assert!(!valid_webhook_secret(&headers, Some("other")));
    }

    #[test]
    fn message_split_respects_unicode_character_boundaries() {
        assert_eq!(split_message("中文消息", 2), vec!["中文", "消息"]);
    }
}
