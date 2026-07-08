//! # sylvander-channel-dingtalk
//!
//! DingTalk (钉钉) bot channel — connects DingTalk group chats to
//! the Sylvander agent system via the [`Channel`] trait.
//!
//! # Protocol
//!
//! **Incoming**: DingTalk POSTs callback JSON to our HTTP endpoint.
//! We parse it, map the conversation to a session, and publish a
//! normalized [`BusMessage`](sylvander_agent::bus::BusMessage).
//!
//! **Outgoing**: We subscribe to bus events and send responses via
//! DingTalk webhook (POST to `sessionWebhook` URL included in each
//! incoming message).
//!
//! # Session mapping
//!
//! DingTalk `conversationId` → internal [`SessionId`]. Metadata
//! (webhook URL, sender info) is stored in `external_meta` on the
//! session — agents never see it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use sylvander_agent::bus::{
    BusMessage, MessageKind, StreamEvent, SubscriptionFilter,
};
use sylvander_agent::session::SessionMetadata;
use sylvander_agent::session_store::{SessionLifetime, SessionStore, StoredSession};
use sylvander_agent::spec::SessionId;
use sylvander_channel::{Channel, ChannelContext};
use tracing::{info, warn};

// ---------------------------------------------------------------------------
// DingTalk message types
// ---------------------------------------------------------------------------

/// Incoming DingTalk callback payload.
#[derive(Debug, Deserialize)]
struct DingTalkCallback {
    /// Unique conversation identifier.
    #[serde(rename = "conversationId")]
    conversation_id: String,
    /// DingTalk user ID of the sender.
    #[serde(rename = "senderId")]
    sender_id: String,
    /// Sender nickname (optional).
    #[serde(rename = "senderNick", default)]
    sender_nick: String,
    /// Webhook URL for sending replies.
    #[serde(rename = "sessionWebhook")]
    session_webhook: String,
    /// Message content.
    text: DingTalkText,
}

#[derive(Debug, Deserialize)]
struct DingTalkText {
    content: String,
}

/// Outgoing webhook message (text).
#[derive(Debug, Serialize)]
struct OutgoingText {
    msgtype: String,
    text: OutgoingTextContent,
}

#[derive(Debug, Serialize)]
struct OutgoingTextContent {
    content: String,
}

/// Outgoing webhook message (markdown).
#[derive(Debug, Serialize)]
struct OutgoingMarkdown {
    msgtype: String,
    markdown: OutgoingMarkdownContent,
}

#[derive(Debug, Serialize)]
struct OutgoingMarkdownContent {
    title: String,
    text: String,
}

// ---------------------------------------------------------------------------
// DingTalkChannel
// ---------------------------------------------------------------------------

/// A DingTalk bot channel.
///
/// Listens for incoming webhook callbacks on an HTTP endpoint and
/// sends responses via DingTalk webhook.
pub struct DingTalkChannel {
    /// HTTP listen address.
    listen_addr: SocketAddr,
}

impl DingTalkChannel {
    /// Create a new DingTalk channel.
    #[must_use]
    pub fn new(listen_addr: SocketAddr) -> Self {
        Self { listen_addr }
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn name(&self) -> &str {
        "dingtalk"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);
        let app_state = Arc::new(AppState {
            ctx: ctx.clone(),
        });

        // Spawn bus listener for outgoing events
        let outgoing_ctx = ctx.clone();
        tokio::spawn(async move {
            run_outgoing_loop(outgoing_ctx).await;
        });

        // Start HTTP server for incoming callbacks
        let app = Router::new()
            .route("/dingtalk/callback", post(handle_callback))
            .with_state(app_state);

        let listener = tokio::net::TcpListener::bind(self.listen_addr)
            .await
            .expect("failed to bind dingtalk listener");

        info!(addr = %self.listen_addr, "dingtalk channel listening");
        axum::serve(listener, app).await.expect("dingtalk server error");
    }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct AppState {
    ctx: Arc<ChannelContext>,
}

// ---------------------------------------------------------------------------
// Incoming: DingTalk callback → BusMessage
// ---------------------------------------------------------------------------

async fn handle_callback(
    State(state): State<Arc<AppState>>,
    Json(cb): Json<DingTalkCallback>,
) -> &'static str {
    // 1. Map conversation → session
    let session_id = resolve_session(&state.ctx, &cb).await;

    // 2. Publish normalized message
    let msg = BusMessage::user_chat(
        session_id.clone(),
        cb.sender_id.clone(),
        cb.text.content.clone(),
    );

    if let Err(e) = state.ctx.bus.publish(msg).await {
        warn!(error = %e, "failed to publish dingtalk message");
        return "error";
    }

    info!(
        session_id = %session_id,
        sender = %cb.sender_id,
        text = %cb.text.content,
        "dingtalk message received"
    );

    "ok"
}

/// Map DingTalk conversationId to a session. Creates one if new.
async fn resolve_session(ctx: &ChannelContext, cb: &DingTalkCallback) -> SessionId {
    // Look for existing session by external meta
    let existing = find_session_by_conversation(&ctx.sessions, &cb.conversation_id).await;

    if let Some(sid) = existing {
        return sid;
    }

    // Create new session
    let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
    let meta = SessionMetadata {
        workspace: "/tmp".into(),
        name: format!("dingtalk-{}", &cb.conversation_id[..8.min(cb.conversation_id.len())]),
        user_id: cb.sender_id.clone(),
    };

    let session_name = meta.name.clone();
    let stored = StoredSession::new(
        session_id.clone(),
        session_name,
        SessionLifetime::Persistent,
        meta,
        vec![], // agents will be joined by the runtime
    )
    .with_external_meta("conversation_id", cb.conversation_id.clone())
    .with_external_meta("session_webhook", cb.session_webhook.clone());

    if let Err(e) = ctx.sessions.save(&stored).await {
        warn!(error = %e, "failed to save dingtalk session");
    }

    info!(session_id = %session_id, conversation_id = %cb.conversation_id, "created dingtalk session");
    session_id
}

async fn find_session_by_conversation(
    store: &Arc<dyn SessionStore>,
    conversation_id: &str,
) -> Option<SessionId> {
    let persistent = store.list_persistent().await.ok()?;
    for s in &persistent {
        if s.external_meta
            .get("conversation_id")
            .and_then(|v| v.as_str())
            == Some(conversation_id)
        {
            return Some(s.id.clone());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Outgoing: bus events → DingTalk webhook
// ---------------------------------------------------------------------------

async fn run_outgoing_loop(ctx: Arc<ChannelContext>) {
    let mut rx = match ctx
        .bus
        .subscribe(SubscriptionFilter::all())
        .await
    {
        Ok(rx) => rx,
        Err(e) => {
            warn!(error = %e, "dingtalk failed to subscribe to bus");
            return;
        }
    };

    // Per-session text accumulator (for streaming text → edited message)
    let mut accumulators: HashMap<SessionId, String> = HashMap::new();

    while let Some(msg) = rx.recv().await {
        match &msg.kind {
            MessageKind::Stream(stream_event) => {
                handle_stream_event(
                    &ctx,
                    &msg.session_id,
                    stream_event,
                    &mut accumulators,
                )
                .await;
            }
            _ => {}
        }
    }
}

async fn handle_stream_event(
    ctx: &ChannelContext,
    session_id: &SessionId,
    event: &StreamEvent,
    accumulators: &mut HashMap<SessionId, String>,
) {
    // Get webhook URL from session metadata
    let webhook_url = get_session_webhook(&ctx.sessions, session_id).await;
    let Some(webhook_url) = webhook_url else {
        return;
    };

    match event {
        StreamEvent::TextDelta { delta } => {
            let acc = accumulators.entry(session_id.clone()).or_default();
            acc.push_str(delta);
            // DingTalk doesn't support message editing — just send.
            // For a better UX, we could batch or debounce.
        }

        StreamEvent::ToolCall {
            call_id: _,
            tool_name,
            input: _,
        } => {
            send_text(&webhook_url, &format!("🔧 调用工具: {tool_name}")).await;
        }

        StreamEvent::ToolResult {
            call_id: _,
            tool_name,
            output,
            is_error,
        } => {
            let prefix = if *is_error { "❌" } else { "✅" };
            let summary = if output.len() > 200 {
                format!("{}...", &output[..200])
            } else {
                output.clone()
            };
            send_text(&webhook_url, &format!("{prefix} {tool_name}: {summary}")).await;
        }

        StreamEvent::Done { text } => {
            // Remove accumulator, send final message
            accumulators.remove(session_id);
            send_markdown(&webhook_url, "Agent 回复", text).await;
        }

        StreamEvent::ToolApprovalRequired { tools, .. } => {
            let tool_list: Vec<String> = tools
                .iter()
                .map(|t| format!("- `{}`", t.tool_name))
                .collect();
            let msg = format!(
                "⚠️ 需要审批以下工具调用:\n{}\n请回复 `approve <call_id>` 或 `reject <call_id>`",
                tool_list.join("\n")
            );
            send_text(&webhook_url, &msg).await;
        }

        StreamEvent::IterationStart { iteration } => {
            send_text(&webhook_url, &format!("💭 思考中... (第 {iteration} 轮)")).await;
        }

        _ => {}
    }
}

async fn get_session_webhook(
    store: &Arc<dyn SessionStore>,
    session_id: &SessionId,
) -> Option<String> {
    let session = store.get(session_id).await.ok()??;
    session
        .external_meta
        .get("session_webhook")
        .and_then(|v| v.as_str())
        .map(String::from)
}

async fn send_text(webhook_url: &str, text: &str) {
    let msg = OutgoingText {
        msgtype: "text".into(),
        text: OutgoingTextContent {
            content: text.to_string(),
        },
    };
    let _ = reqwest::Client::new()
        .post(webhook_url)
        .json(&msg)
        .send()
        .await;
}

async fn send_markdown(webhook_url: &str, title: &str, text: &str) {
    let msg = OutgoingMarkdown {
        msgtype: "markdown".into(),
        markdown: OutgoingMarkdownContent {
            title: title.into(),
            text: text.to_string(),
        },
    };
    let _ = reqwest::Client::new()
        .post(webhook_url)
        .json(&msg)
        .send()
        .await;
}
