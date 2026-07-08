//! # sylvander-channel-dingtalk
//!
//! DingTalk bot channel. Two-layer architecture:
//!
//! ```text
//! lib.rs — Channel trait impl (glue: session mapping, bus pub/sub)
//!   ↓
//! protocol.rs — DingTalk Stream protocol (pure SDK, no Sylvander deps)
//!   Client, RobotMessage, MessageHandler
//! ```

pub mod protocol;

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, warn};

use sylvander_agent::bus::{BusMessage, MessageKind, SubscriptionFilter};
use sylvander_agent::session::SessionMetadata;
use sylvander_agent::session_store::{SessionLifetime, SessionStore, StoredSession};
use sylvander_agent::spec::SessionId;
use sylvander_channel::{Channel, ChannelContext};

use protocol::{Client, FrameHeaders, MessageHandler, RobotMessage};

// ===========================================================================
// ChannelMessageHandler — bridges protocol → Sylvander
// ===========================================================================

struct ChannelMessageHandler {
    ctx: Arc<ChannelContext>,
}

#[async_trait]
impl MessageHandler for ChannelMessageHandler {
    async fn on_message(&self, msg: &RobotMessage, _headers: &FrameHeaders) {
        let session_id = resolve_session(&self.ctx, msg).await;

        let text = msg
            .text
            .as_ref()
            .map(|t| t.content.as_str())
            .unwrap_or("");

        let bus_msg = BusMessage::user_chat(session_id.clone(), &msg.sender_staff_id, text);

        if let Err(e) = self.ctx.bus.publish(bus_msg).await {
            warn!(error = %e, "dingtalk: bus publish failed");
            return;
        }

        info!(%session_id, sender = %msg.sender_staff_id, text, "dingtalk: message");
    }
}

// ===========================================================================
// Session mapping
// ===========================================================================

async fn resolve_session(ctx: &ChannelContext, msg: &RobotMessage) -> SessionId {
    if let Some(sid) = find_by_conversation_id(&ctx.sessions, &msg.conversation_id).await {
        return sid;
    }

    let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
    let meta = SessionMetadata {
        workspace: "/tmp".into(),
        name: format!("dt-{}", &msg.conversation_id[..8.min(msg.conversation_id.len())]),
        user_id: msg.sender_staff_id.clone(),
    };
    let session_name = meta.name.clone();

    let stored = StoredSession::new(session_id.clone(), session_name, SessionLifetime::Persistent, meta, vec![])
        .with_external_meta("conversation_id", msg.conversation_id.clone())
        .with_external_meta("session_webhook", msg.session_webhook.clone());

    if let Err(e) = ctx.sessions.save(&stored).await {
        warn!(error = %e, "dingtalk: save session failed");
    }

    info!(%session_id, conv_id = %msg.conversation_id, "dingtalk: session created");
    session_id
}

async fn find_by_conversation_id(store: &Arc<dyn SessionStore>, conv_id: &str) -> Option<SessionId> {
    let persistent = store.list_persistent().await.ok()?;
    for s in &persistent {
        if s.external_meta.get("conversation_id").and_then(|v| v.as_str()) == Some(conv_id) {
            return Some(s.id.clone());
        }
    }
    None
}

// ===========================================================================
// Outgoing: bus events → DingTalk webhook
// ===========================================================================

async fn run_outgoing(ctx: Arc<ChannelContext>, client: Client) {
    let mut rx = match ctx.bus.subscribe(SubscriptionFilter::all()).await {
        Ok(rx) => rx,
        Err(e) => {
            warn!(error = %e, "dingtalk: outgoing subscribe failed");
            return;
        }
    };

    while let Some(msg) = rx.recv().await {
        let MessageKind::Stream(ref ev) = msg.kind else { continue };

        let webhook_url = get_webhook_url(&ctx.sessions, &msg.session_id).await;
        let Some(ref url) = webhook_url else { continue };

        match ev {
            sylvander_agent::bus::StreamEvent::Done { text } => {
                client.reply_markdown(url, "Reply", text).await;
            }
            sylvander_agent::bus::StreamEvent::ToolCall { tool_name, .. } => {
                client.reply_text(url, &format!("🔧 calling: {tool_name}")).await;
            }
            sylvander_agent::bus::StreamEvent::ToolApprovalRequired { tools, .. } => {
                let list: Vec<String> = tools.iter().map(|t| format!("- `{}`", t.tool_name)).collect();
                client.reply_text(url, &format!("⚠️ approval needed:\n{}", list.join("\n"))).await;
            }
            sylvander_agent::bus::StreamEvent::IterationStart { iteration } => {
                client.reply_text(url, &format!("💭 thinking... (round {iteration})")).await;
            }
            _ => {}
        }
    }
}

async fn get_webhook_url(store: &Arc<dyn SessionStore>, session_id: &SessionId) -> Option<String> {
    let session = store.get(session_id).await.ok()??;
    session.external_meta.get("session_webhook").and_then(|v| v.as_str()).map(String::from)
}

// ===========================================================================
// Channel impl
// ===========================================================================

/// DingTalk bot channel.
pub struct DingTalkChannel {
    client: Client,
}

impl DingTalkChannel {
    pub fn new(app_key: impl Into<String>, app_secret: impl Into<String>) -> Self {
        Self { client: Client::new(app_key, app_secret) }
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn name(&self) -> &str { "dingtalk" }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);

        // Outgoing loop
        let out_ctx = ctx.clone();
        let out_client = self.client.clone();
        tokio::spawn(async move { run_outgoing(out_ctx, out_client).await });

        // Incoming loop (blocking — runs until WebSocket closes)
        let handler = Arc::new(ChannelMessageHandler { ctx });
        self.client.run(handler).await;
    }
}
