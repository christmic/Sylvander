//! # sylvander-channel-dingtalk
//!
//! DingTalk (钉钉) bot channel. Transport-agnostic — I/O is injected
//! via traits, not hard-coded.
//!
//! ```text
//! DingTalkChannel
//!   ├── Box<dyn IncomingTransport>  ← recv() → DingTalkIncoming
//!   ├── Arc<dyn OutgoingTransport>  ← send(webhook_url, msg)
//!   └── core: parse → session map → normalize → bus publish
//! ```
//!
//! # Built-in transports
//!
//! - [`AxumCallbackServer`] — axum HTTP server for DingTalk callbacks
//! - [`ReqwestWebhook`] — reqwest-based webhook sender

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sylvander_agent::bus::{BusMessage, MessageKind, StreamEvent, SubscriptionFilter};
use sylvander_agent::session::SessionMetadata;
use sylvander_agent::session_store::{SessionLifetime, SessionStore, StoredSession};
use sylvander_agent::spec::SessionId;
use sylvander_channel::{Channel, ChannelContext};
use tracing::{info, warn};

// ===========================================================================
// DingTalk protocol types
// ===========================================================================

/// Incoming DingTalk callback payload.
#[derive(Debug, Clone, Deserialize)]
pub struct DingTalkCallback {
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    #[serde(rename = "senderId")]
    pub sender_id: String,
    #[serde(rename = "senderNick", default)]
    pub sender_nick: String,
    /// Webhook URL for sending replies.
    #[serde(rename = "sessionWebhook")]
    pub session_webhook: String,
    pub text: DingTalkTextContent,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DingTalkTextContent {
    pub content: String,
}

/// Parsed incoming message.
#[derive(Debug, Clone)]
pub struct DingTalkIncoming {
    pub callback: DingTalkCallback,
}

/// Outgoing message.
#[derive(Debug, Clone)]
pub enum DingTalkOutgoing {
    Text { content: String },
    Markdown { title: String, text: String },
}

// ===========================================================================
// Transport traits
// ===========================================================================

/// Receives incoming DingTalk messages.
#[async_trait]
pub trait IncomingTransport: Send + Sync {
    /// Wait for the next message. Returns `None` when the transport
    /// is closed.
    async fn recv(&mut self) -> Option<DingTalkIncoming>;
}

/// Sends outgoing messages to DingTalk webhook URLs.
#[async_trait]
pub trait OutgoingTransport: Send + Sync {
    /// POST a message to the given webhook URL.
    async fn send(&self, webhook_url: &str, msg: &DingTalkOutgoing);
}

// ===========================================================================
// DingTalkChannel
// ===========================================================================

/// A DingTalk bot channel — transport injected via trait objects.
pub struct DingTalkChannel {
    incoming: tokio::sync::Mutex<Box<dyn IncomingTransport>>,
    outgoing: Arc<dyn OutgoingTransport>,
}

impl DingTalkChannel {
    /// Create a new channel with the given transports.
    pub fn new(
        incoming: Box<dyn IncomingTransport>,
        outgoing: Arc<dyn OutgoingTransport>,
    ) -> Self {
        Self {
            incoming: tokio::sync::Mutex::new(incoming),
            outgoing,
        }
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn name(&self) -> &str {
        "dingtalk"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);

        // Outgoing loop: bus events → webhook
        let outgoing_ctx = ctx.clone();
        let outgoing = self.outgoing.clone();
        tokio::spawn(async move { run_outgoing_loop(outgoing_ctx, outgoing).await });

        // Incoming loop: transport.recv() → parse → publish
        let mut incoming = self.incoming.lock().await;
        while let Some(msg) = incoming.recv().await {
            handle_incoming(&ctx, msg).await;
        }
        info!("dingtalk incoming transport closed");
    }
}

// ===========================================================================
// Incoming: transport → parse → session map → bus publish
// ===========================================================================

async fn handle_incoming(ctx: &ChannelContext, msg: DingTalkIncoming) {
    let cb = &msg.callback;
    let session_id = resolve_session(ctx, cb).await;

    let bus_msg = BusMessage::user_chat(
        session_id.clone(),
        cb.sender_id.clone(),
        cb.text.content.clone(),
    );

    if let Err(e) = ctx.bus.publish(bus_msg).await {
        warn!(error = %e, "failed to publish dingtalk message");
        return;
    }

    info!(
        session_id = %session_id,
        sender = %cb.sender_id,
        "dingtalk message received"
    );
}

async fn resolve_session(ctx: &ChannelContext, cb: &DingTalkCallback) -> SessionId {
    if let Some(sid) = find_session_by_conversation(&ctx.sessions, &cb.conversation_id).await {
        return sid;
    }

    let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
    let meta = SessionMetadata {
        workspace: "/tmp".into(),
        name: format!(
            "dingtalk-{}",
            &cb.conversation_id[..8.min(cb.conversation_id.len())]
        ),
        user_id: cb.sender_id.clone(),
    };
    let session_name = meta.name.clone();

    let stored = StoredSession::new(
        session_id.clone(),
        session_name,
        SessionLifetime::Persistent,
        meta,
        vec![],
    )
    .with_external_meta("conversation_id", cb.conversation_id.clone())
    .with_external_meta("session_webhook", cb.session_webhook.clone());

    if let Err(e) = ctx.sessions.save(&stored).await {
        warn!(error = %e, "failed to save session");
    }

    info!(%session_id, conversation_id = %cb.conversation_id, "created dingtalk session");
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

// ===========================================================================
// Outgoing: bus events → transport.send()
// ===========================================================================

async fn run_outgoing_loop(ctx: Arc<ChannelContext>, transport: Arc<dyn OutgoingTransport>) {
    let mut rx = match ctx.bus.subscribe(SubscriptionFilter::all()).await {
        Ok(rx) => rx,
        Err(e) => {
            warn!(error = %e, "dingtalk bus subscribe failed");
            return;
        }
    };

    let mut accumulators: HashMap<SessionId, String> = HashMap::new();

    while let Some(msg) = rx.recv().await {
        if let MessageKind::Stream(ev) = &msg.kind {
            let webhook_url =
                get_session_webhook(&ctx.sessions, &msg.session_id).await;
            let Some(ref url) = webhook_url else { continue };

            handle_stream_event(transport.as_ref(), url, ev, &mut accumulators).await;
        }
    }
}

async fn handle_stream_event(
    transport: &dyn OutgoingTransport,
    webhook_url: &str,
    event: &StreamEvent,
    accumulators: &mut HashMap<SessionId, String>,
) {
    match event {
        StreamEvent::TextDelta { delta } => {
            accumulators
                .entry(SessionId::new("")) // FIXME: use actual session_id
                .or_default()
                .push_str(delta);
        }

        StreamEvent::ToolCall { tool_name, .. } => {
            transport
                .send(webhook_url, &DingTalkOutgoing::Text {
                    content: format!("🔧 调用工具: {tool_name}"),
                })
                .await;
        }

        StreamEvent::ToolResult { tool_name, output, is_error, .. } => {
            let prefix = if *is_error { "❌" } else { "✅" };
            let summary: String = if output.len() > 200 {
                format!("{}...", &output[..200])
            } else {
                output.clone()
            };
            transport
                .send(webhook_url, &DingTalkOutgoing::Text {
                    content: format!("{prefix} {tool_name}: {summary}"),
                })
                .await;
        }

        StreamEvent::Done { text } => {
            transport
                .send(webhook_url, &DingTalkOutgoing::Markdown {
                    title: "Agent 回复".into(),
                    text: text.clone(),
                })
                .await;
        }

        StreamEvent::ToolApprovalRequired { tools, .. } => {
            let tool_list: Vec<String> = tools
                .iter()
                .map(|t| format!("- `{}`", t.tool_name))
                .collect();
            transport
                .send(webhook_url, &DingTalkOutgoing::Text {
                    content: format!(
                        "⚠️ 需要审批:\n{}\n回复 `approve <id>` 或 `reject <id>`",
                        tool_list.join("\n")
                    ),
                })
                .await;
        }

        StreamEvent::IterationStart { iteration } => {
            transport
                .send(webhook_url, &DingTalkOutgoing::Text {
                    content: format!("💭 思考中... (第 {iteration} 轮)"),
                })
                .await;
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

// ===========================================================================
// AxumCallbackServer (default HTTP transport)
// ===========================================================================

/// Receives DingTalk callbacks via an axum HTTP server.
pub struct AxumCallbackServer {
    rx: tokio::sync::mpsc::UnboundedReceiver<DingTalkIncoming>,
}

impl AxumCallbackServer {
    /// Bind an axum server and spawn it in a background task.
    /// Returns the transport and the bound address.
    pub async fn bind(addr: SocketAddr) -> (Self, SocketAddr) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();

        let app = axum::Router::new().route(
            "/dingtalk/callback",
            axum::routing::post(|axum::Json(cb): axum::Json<DingTalkCallback>| async move {
                tx.send(DingTalkIncoming { callback: cb }).ok();
                "ok"
            }),
        );

        let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
        let bound_addr = listener.local_addr().expect("local addr");

        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });

        (Self { rx }, bound_addr)
    }
}

#[async_trait]
impl IncomingTransport for AxumCallbackServer {
    async fn recv(&mut self) -> Option<DingTalkIncoming> {
        self.rx.recv().await
    }
}

// ===========================================================================
// ReqwestWebhook (default HTTP client transport)
// ===========================================================================

/// Sends messages via DingTalk webhook using reqwest.
#[derive(Clone)]
pub struct ReqwestWebhook {
    client: reqwest::Client,
}

impl ReqwestWebhook {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestWebhook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OutgoingTransport for ReqwestWebhook {
    async fn send(&self, webhook_url: &str, msg: &DingTalkOutgoing) {
        let result = match msg {
            DingTalkOutgoing::Text { content } => {
                #[derive(Serialize)]
                struct Payload {
                    msgtype: String,
                    text: TextPayload,
                }
                #[derive(Serialize)]
                struct TextPayload {
                    content: String,
                }
                self.client
                    .post(webhook_url)
                    .json(&Payload {
                        msgtype: "text".into(),
                        text: TextPayload {
                            content: content.clone(),
                        },
                    })
                    .send()
                    .await
            }
            DingTalkOutgoing::Markdown { title, text } => {
                #[derive(Serialize)]
                struct Payload {
                    msgtype: String,
                    markdown: MarkdownPayload,
                }
                #[derive(Serialize)]
                struct MarkdownPayload {
                    title: String,
                    text: String,
                }
                self.client
                    .post(webhook_url)
                    .json(&Payload {
                        msgtype: "markdown".into(),
                        markdown: MarkdownPayload {
                            title: title.clone(),
                            text: text.clone(),
                        },
                    })
                    .send()
                    .await
            }
        };
        if let Err(e) = result {
            warn!(error = %e, "dingtalk webhook send failed");
        }
    }
}
