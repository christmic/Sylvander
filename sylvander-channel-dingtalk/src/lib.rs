//! # sylvander-channel-dingtalk
//!
//! `DingTalk` bot channel. Two-layer architecture:
//!
//! ```text
//! lib.rs — Channel trait impl (glue: session mapping, bus pub/sub)
//!   ↓
//! protocol.rs — DingTalk Stream protocol (pure SDK, no Sylvander deps)
//!   Client, RobotMessage, MessageHandler
//! ```

pub mod protocol;

pub use protocol::{FrameHeaders, MessageHandler, ROBOT_TOPIC, RobotMessage};

pub use protocol::Client as DingTalkClient;

/// Parsed incoming message (alias of `RobotMessage` for legacy tests).
pub type DingTalkCallback = RobotMessage;

/// Wraps `RobotMessage` for transport layer.
#[derive(Debug, Clone)]
pub struct DingTalkIncoming {
    pub callback: RobotMessage,
}

/// Placeholder for outgoing (`DingTalk` replies via webhook, not via transport trait).
#[derive(Debug, Clone)]
pub struct DingTalkOutgoing {
    pub kind: String,
    pub text: String,
}

/// Plain-text content.
pub type DingTalkTextContent = protocol::TextContent;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{info, warn};

use sylvander_agent::bus::{MessageKind, SubscriptionFilter};
use sylvander_agent::session_store::SessionStore;
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{Channel, ChannelContext, ExternalChatRequest, submit_external_chat};
use sylvander_protocol::{AuthenticatedPrincipal, AuthenticationMethod, BoundaryContext};

use protocol::Client;

// ===========================================================================
// ChannelMessageHandler — bridges protocol → Sylvander
// ===========================================================================

struct ChannelMessageHandler {
    ctx: Arc<ChannelContext>,
    instance_id: String,
    agent_id: AgentId,
    replay: Arc<ReplayCache>,
}

#[async_trait]
impl MessageHandler for ChannelMessageHandler {
    async fn on_message(&self, msg: &RobotMessage, _headers: &FrameHeaders) {
        if msg.msg_id.is_empty() {
            warn!("dingtalk: ignored message without a stable id");
            return;
        }
        if !self.replay.claim(&msg.msg_id).await {
            info!(message_id = %msg.msg_id, "dingtalk: ignored duplicate message");
            return;
        }
        let text = msg.text.as_ref().map_or("", |t| t.content.as_str());
        let existing = find_by_conversation_id(
            &self.ctx.sessions,
            &self.instance_id,
            &msg.conversation_id,
            &msg.sender_staff_id,
        )
        .await;
        let principal_id = platform_principal_id(&self.instance_id, &msg.sender_staff_id);
        let boundary = BoundaryContext::authenticated(
            AuthenticatedPrincipal::user(
                principal_id.clone(),
                AuthenticationMethod::PlatformIdentity,
            ),
            &self.instance_id,
            "dingtalk",
            format!("dingtalk-message-{}", msg.msg_id),
        );
        let external_meta = BTreeMap::from([
            ("channel_instance_id".into(), self.instance_id.clone()),
            ("conversation_id".into(), msg.conversation_id.clone()),
            ("sender_staff_id".into(), msg.sender_staff_id.clone()),
            ("session_webhook".into(), msg.session_webhook.clone()),
        ]);
        let submitted = match submit_external_chat(
            &self.ctx,
            &boundary,
            ExternalChatRequest {
                existing_session: existing,
                agent_id: self.agent_id.clone(),
                label: format!(
                    "dt-{}",
                    &msg.conversation_id[..8.min(msg.conversation_id.len())]
                ),
                overrides: sylvander_protocol::SessionConfigOverrides::default(),
                text: text.into(),
                attachments: Vec::new(),
                external_meta,
            },
        )
        .await
        {
            Ok(submitted) => submitted,
            Err(error) => {
                warn!(code = ?error.code, request_id = %error.request_id, "dingtalk: message denied");
                return;
            }
        };
        let session_id = submitted.session_id;
        drop(submitted.events);

        info!(%session_id, sender = %msg.sender_staff_id, text, "dingtalk: message");
    }
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

fn platform_principal_id(instance_id: &str, sender_staff_id: &str) -> String {
    format!("dingtalk:{instance_id}:{sender_staff_id}")
}

// ===========================================================================
// Session mapping
// ===========================================================================

async fn find_by_conversation_id(
    store: &Arc<dyn SessionStore>,
    instance_id: &str,
    conv_id: &str,
    sender_staff_id: &str,
) -> Option<SessionId> {
    let persistent = store.list_persistent().await.ok()?;
    for s in &persistent {
        if s.external_meta
            .get("channel_instance_id")
            .and_then(|v| v.as_str())
            == Some(instance_id)
            && s.external_meta
                .get("conversation_id")
                .and_then(|v| v.as_str())
                == Some(conv_id)
            && s.external_meta
                .get("sender_staff_id")
                .and_then(|v| v.as_str())
                == Some(sender_staff_id)
        {
            return Some(s.id.clone());
        }
    }
    None
}

// ===========================================================================
// Outgoing: bus events → DingTalk webhook
// ===========================================================================

async fn run_outgoing(
    ctx: Arc<ChannelContext>,
    client: Client,
    instance_id: String,
    agent_id: AgentId,
) {
    let mut rx = match ctx.subscribe(SubscriptionFilter::for_agent(agent_id)).await {
        Ok(rx) => rx,
        Err(e) => {
            warn!(error = %e, "dingtalk: outgoing subscribe failed");
            return;
        }
    };

    while let Some(msg) = rx.recv().await {
        let MessageKind::Stream(ref ev) = msg.kind else {
            continue;
        };

        let webhook_url = get_webhook_url(&ctx.sessions, &msg.session_id, &instance_id).await;
        let Some(ref url) = webhook_url else { continue };

        match ev {
            sylvander_agent::bus::StreamEvent::Done { text } => {
                client.reply_markdown(url, "Reply", text).await;
            }
            sylvander_agent::bus::StreamEvent::ToolCall { tool_name, .. } => {
                client
                    .reply_text(url, &format!("🔧 calling: {tool_name}"))
                    .await;
            }
            sylvander_agent::bus::StreamEvent::ToolApprovalRequired { tools, .. } => {
                let list: Vec<String> = tools
                    .iter()
                    .map(|t| format!("- `{}`", t.tool_name))
                    .collect();
                client
                    .reply_text(url, &format!("⚠️ approval needed:\n{}", list.join("\n")))
                    .await;
            }
            sylvander_agent::bus::StreamEvent::IterationStart { iteration } => {
                client
                    .reply_text(url, &format!("💭 thinking... (round {iteration})"))
                    .await;
            }
            _ => {}
        }
    }
}

async fn get_webhook_url(
    store: &Arc<dyn SessionStore>,
    session_id: &SessionId,
    instance_id: &str,
) -> Option<String> {
    let session = store.get(session_id).await.ok()??;
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
        .get("session_webhook")
        .and_then(|v| v.as_str())
        .map(String::from)
}

// ===========================================================================
// Channel impl
// ===========================================================================

/// `DingTalk` bot channel.
pub struct DingTalkChannel {
    client: Client,
    instance_id: String,
    agent_id: AgentId,
}

impl DingTalkChannel {
    pub fn new(app_key: impl Into<String>, app_secret: impl Into<String>) -> Self {
        Self {
            client: Client::new(app_key, app_secret),
            instance_id: "dingtalk".into(),
            agent_id: AgentId::new("default"),
        }
    }

    /// Bind this channel to its configured instance and default Agent.
    #[must_use]
    pub fn with_identity(
        mut self,
        instance_id: impl Into<String>,
        agent_id: impl Into<AgentId>,
    ) -> Self {
        self.instance_id = instance_id.into();
        self.agent_id = agent_id.into();
        self
    }

    #[must_use]
    pub fn with_request_limit(mut self, max_request_bytes: usize) -> Self {
        self.client = self.client.with_message_limit(max_request_bytes);
        self
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn name(&self) -> &'static str {
        "dingtalk"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);

        // Outgoing loop
        let out_ctx = ctx.clone();
        let out_client = self.client.clone();
        let out_instance_id = self.instance_id.clone();
        let out_agent_id = self.agent_id.clone();
        let outgoing = tokio::spawn(async move {
            run_outgoing(out_ctx, out_client, out_instance_id, out_agent_id).await;
        });

        // Incoming loop (blocking — runs until WebSocket closes)
        let handler = Arc::new(ChannelMessageHandler {
            ctx,
            instance_id: self.instance_id.clone(),
            agent_id: self.agent_id.clone(),
            replay: Arc::new(ReplayCache::default()),
        });
        handler.ctx.mark_ready();
        tokio::select! {
            () = self.client.run(handler.clone()) => {}
            () = handler.ctx.shutdown_requested() => {}
        }
        outgoing.abort();
        let _ = outgoing.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_agent::session::SessionMetadata;
    use sylvander_agent::session_store::SqliteSessionStore;
    use sylvander_agent::session_store::{SessionLifetime, StoredSession};

    #[test]
    fn request_limit_is_configurable() {
        let channel = DingTalkChannel::new("key", "secret").with_request_limit(4096);
        assert_eq!(channel.client.max_message_bytes(), 4096);
    }

    #[tokio::test]
    async fn conversation_lookup_requires_instance_and_sender() {
        let store: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
        let session_id = SessionId::new("session-a");
        let stored = StoredSession::new(
            session_id.clone(),
            "test",
            SessionLifetime::Persistent,
            SessionMetadata {
                workspace: "/tmp".into(),
                name: "test".into(),
                user_id: "dingtalk:bot-a:user-a".into(),
            },
            vec![AgentId::new("agent-a")],
        )
        .with_external_meta("channel_instance_id", "bot-a")
        .with_external_meta("conversation_id", "conversation-a")
        .with_external_meta("sender_staff_id", "user-a")
        .with_external_meta("session_webhook", "https://example.invalid/reply");
        store.save(&stored).await.unwrap();

        assert_eq!(
            find_by_conversation_id(&store, "bot-a", "conversation-a", "user-a").await,
            Some(session_id.clone())
        );
        assert!(
            find_by_conversation_id(&store, "bot-b", "conversation-a", "user-a")
                .await
                .is_none()
        );
        assert!(
            find_by_conversation_id(&store, "bot-a", "conversation-a", "user-b")
                .await
                .is_none()
        );
        assert!(
            get_webhook_url(&store, &session_id, "bot-b")
                .await
                .is_none()
        );
    }

    #[test]
    fn principal_identity_includes_instance_and_sender() {
        assert_eq!(
            platform_principal_id("bot-a", "user-a"),
            "dingtalk:bot-a:user-a"
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
