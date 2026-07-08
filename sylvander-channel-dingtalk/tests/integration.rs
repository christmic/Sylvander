//! Tests for DingTalkChannel — using mock transports, no real HTTP.
//!
//! We inject a mock IncomingTransport (pushes a DingTalkCallback)
//! and a mock OutgoingTransport (records sent messages).

use std::sync::Arc;

use async_trait::async_trait;
use sylvander_agent::bus::{InProcessMessageBus, MessageKind, SubscriptionFilter};
use sylvander_agent::session_store::{InMemorySessionStore, SessionStore};
use sylvander_channel::{Channel, ChannelContext};
use sylvander_channel_dingtalk::{
    DingTalkCallback, DingTalkChannel, DingTalkIncoming, DingTalkOutgoing,
    DingTalkTextContent, IncomingTransport, OutgoingTransport,
};

// ---- mock transports ----

/// Mock incoming transport — push a message via mpsc.
struct MockIncoming {
    rx: tokio::sync::mpsc::UnboundedReceiver<DingTalkIncoming>,
}

impl MockIncoming {
    fn new() -> (Self, tokio::sync::mpsc::UnboundedSender<DingTalkIncoming>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { rx }, tx)
    }
}

#[async_trait]
impl IncomingTransport for MockIncoming {
    async fn recv(&mut self) -> Option<DingTalkIncoming> {
        self.rx.recv().await
    }
}

/// Mock outgoing transport — records all sent messages.
struct MockOutgoing {
    sent: tokio::sync::Mutex<Vec<(String, DingTalkOutgoing)>>,
}

impl MockOutgoing {
    fn new() -> Self {
        Self {
            sent: tokio::sync::Mutex::new(Vec::new()),
        }
    }

    async fn take(&self) -> Vec<(String, DingTalkOutgoing)> {
        std::mem::take(&mut *self.sent.lock().await)
    }
}

#[async_trait]
impl OutgoingTransport for MockOutgoing {
    async fn send(&self, webhook_url: &str, msg: &DingTalkOutgoing) {
        self.sent
            .lock()
            .await
            .push((webhook_url.to_string(), msg.clone()));
    }
}

// ---- helpers ----

fn dingtalk_callback(conversation_id: &str, text: &str, webhook: &str) -> DingTalkCallback {
    DingTalkCallback {
        conversation_id: conversation_id.into(),
        sender_id: "user-1".into(),
        sender_nick: "Alice".into(),
        session_webhook: webhook.into(),
        text: DingTalkTextContent {
            content: text.into(),
        },
    }
}

async fn setup() -> (
    Arc<DingTalkChannel>,
    ChannelContext,
    tokio::sync::mpsc::UnboundedSender<DingTalkIncoming>,
    Arc<MockOutgoing>,
) {
    let bus = Arc::new(InProcessMessageBus::new());
    let sessions: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::new());

    let (mock_in, incoming_tx) = MockIncoming::new();
    let mock_out = Arc::new(MockOutgoing::new());

    let channel = Arc::new(DingTalkChannel::new(
        Box::new(mock_in),
        mock_out.clone(),
    ));

    let ctx = ChannelContext {
        bus: bus.clone(),
        sessions,
    };

    (channel, ctx, incoming_tx, mock_out)
}

// ---- tests ----

#[tokio::test]
async fn incoming_message_published_to_bus() {
    let (channel, ctx, incoming_tx, _out) = setup().await;

    // Subscribe to bus
    let mut bus_rx = ctx
        .bus
        .subscribe(SubscriptionFilter::all())
        .await
        .expect("subscribe");

    // Spawn channel
    let ch = channel.clone();
    tokio::spawn(async move { ch.run(ctx).await });

    // Push a DingTalk message
    incoming_tx
        .send(DingTalkIncoming {
            callback: dingtalk_callback("conv-123", "帮我查日志", "https://webhook.example.com"),
        })
        .ok();

    // Verify bus received it
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), bus_rx.recv())
        .await
        .expect("timeout")
        .expect("should receive");

    assert!(matches!(msg.kind, MessageKind::Chat));
    assert_eq!(msg.payload, "帮我查日志");
}

#[tokio::test]
async fn outgoing_events_sent_via_transport() {
    let (channel, ctx, _incoming_tx, mock_out) = setup().await;

    // Pre-create a session with webhook metadata
    use sylvander_agent::session::SessionMetadata;
    use sylvander_agent::session_store::{SessionLifetime, StoredSession};
    use sylvander_agent::spec::SessionId;
    use std::path::PathBuf;

    let session_id = SessionId::new("test-session");
    let meta = SessionMetadata {
        workspace: PathBuf::from("/tmp"),
        name: "test".into(),
        user_id: "user-1".into(),
    };
    let stored = StoredSession::new(session_id.clone(), "test", SessionLifetime::Persistent, meta, vec![])
        .with_external_meta("session_webhook", "https://hook.example.com");

    ctx.sessions.save(&stored).await.expect("save");

    // Spawn channel
    let bus = ctx.bus.clone();
    let ch = channel.clone();
    let ctx2 = ctx.clone();
    tokio::spawn(async move { ch.run(ctx2).await });

    // Give the outgoing loop time to subscribe
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Publish a stream event
    use sylvander_agent::bus::{BusMessage, StreamEvent};
    use sylvander_agent::spec::AgentId;

    bus.publish(BusMessage::stream_event(
            session_id.clone(),
            AgentId::new("agent-1"),
            StreamEvent::Done {
                text: "查到 3 条错误".into(),
            },
        ))
        .await
        .expect("publish");

    // Wait for the outgoing loop to process
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    let sent = mock_out.take().await;
    assert!(!sent.is_empty(), "should have sent at least one message");

    let (url, msg) = &sent[0];
    assert_eq!(url, "https://hook.example.com");
    assert!(matches!(msg, DingTalkOutgoing::Markdown { .. }));
}

#[tokio::test]
async fn callback_parsing() {
    let json = serde_json::json!({
        "conversationId": "conv-abc",
        "senderId": "user-xyz",
        "senderNick": "Alice",
        "sessionWebhook": "https://oapi.dingtalk.com/robot/send?access_token=xxx",
        "text": { "content": "你好" }
    });

    let cb: DingTalkCallback = serde_json::from_value(json).expect("parse");
    assert_eq!(cb.conversation_id, "conv-abc");
    assert_eq!(cb.sender_id, "user-xyz");
    assert_eq!(cb.text.content, "你好");
}

#[tokio::test]
async fn session_mapping_reuses_existing() {
    let (channel, ctx, incoming_tx, _out) = setup().await;

    // Spawn channel
    let bus = ctx.bus.clone();
    let ch = channel.clone();
    let ctx2 = ctx.clone();
    tokio::spawn(async move { ch.run(ctx2).await });

    // Subscribe to bus
    let mut bus_rx = ctx
        .bus
        .subscribe(SubscriptionFilter::all())
        .await
        .expect("subscribe");

    // Send first message
    incoming_tx
        .send(DingTalkIncoming {
            callback: dingtalk_callback("conv-123", "first", "https://hook.example.com"),
        })
        .ok();
    let msg1 = tokio::time::timeout(std::time::Duration::from_secs(2), bus_rx.recv())
        .await
        .expect("timeout")
        .expect("msg1");
    let sid1 = msg1.session_id.clone();

    // Send second message from same conversation
    incoming_tx
        .send(DingTalkIncoming {
            callback: dingtalk_callback("conv-123", "second", "https://hook.example.com"),
        })
        .ok();
    let msg2 = tokio::time::timeout(std::time::Duration::from_secs(2), bus_rx.recv())
        .await
        .expect("timeout")
        .expect("msg2");

    // Same session
    assert_eq!(msg2.session_id, sid1, "same conversation should reuse session");
}
