//! Message bus — the communication layer between users, agents, and the engine.
//!
//! The bus is a pub/sub channel. Publishers emit [`BusMessage`]s that are
//! routed to subscribers based on [`SubscriptionFilter`] matching.
//!
//! The default implementation ([`InProcessMessageBus`]) uses tokio channels
//! and is suitable for single-process deployments. A persistent bus (Redis,
//! NATS) can be swapped in via the [`MessageBus`] trait.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use crate::spec::{AgentId, SessionId};

// ---------------------------------------------------------------------------
// MessageId
// ---------------------------------------------------------------------------

/// Unique identifier for a bus message. Used for idempotency and tracing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageId(pub Uuid);

impl MessageId {
    /// Generate a new random message ID.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

/// Who sent the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender {
    /// A human user.
    User(String),
    /// An agent.
    Agent(AgentId),
    /// The system / engine itself.
    System,
}

/// Who should receive the message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Recipient {
    /// Deliver to a specific agent.
    Agent(AgentId),
    /// Deliver to all participants in the session.
    Broadcast,
}

/// What kind of message this is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// A normal chat message (user ↔ agent).
    Chat,
    /// A lifecycle control command.
    Control(ControlAction),
}

/// Lifecycle control action.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ControlAction {
    /// Pause the agent.
    Pause,
    /// Resume a paused agent.
    Resume,
    /// Stop / despawn the agent.
    Stop,
}

/// A message on the bus.
#[derive(Debug, Clone)]
pub struct BusMessage {
    /// Which session this message belongs to.
    pub session_id: SessionId,
    /// Who sent the message.
    pub sender: Sender,
    /// Intended recipient(s).
    pub recipient: Recipient,
    /// Message category.
    pub kind: MessageKind,
    /// Message body (plain text or JSON).
    pub payload: String,
    /// Unix timestamp in seconds.
    pub timestamp: i64,
    /// Unique message ID.
    pub id: MessageId,
}

impl BusMessage {
    /// Create a simple chat message from a user.
    #[must_use]
    pub fn user_chat(
        session_id: SessionId,
        user_id: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            sender: Sender::User(user_id.into()),
            recipient: Recipient::Broadcast,
            kind: MessageKind::Chat,
            payload: text.into(),
            timestamp: crate::session::now_secs(),
            id: MessageId::new(),
        }
    }

    /// Create a response message from an agent.
    #[must_use]
    pub fn agent_response(
        session_id: SessionId,
        agent_id: AgentId,
        text: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            sender: Sender::Agent(agent_id),
            recipient: Recipient::Broadcast,
            kind: MessageKind::Chat,
            payload: text.into(),
            timestamp: crate::session::now_secs(),
            id: MessageId::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// SubscriptionFilter
// ---------------------------------------------------------------------------

/// Filter that determines which messages a subscriber receives.
///
/// All `Option` fields are conjunctive: if a field is `Some`, the
/// message must match at least one value in the list. If `None`, the
/// field is ignored (matches everything).
#[derive(Debug, Clone)]
pub struct SubscriptionFilter {
    /// Only match messages from these sessions. `None` matches all.
    pub session_ids: Option<Vec<SessionId>>,
    /// Only match messages targeting these recipients. `None` matches all.
    pub recipients: Option<Vec<Recipient>>,
    /// Only match these message kinds. `None` matches all.
    pub kinds: Option<Vec<MessageKind>>,
}

impl SubscriptionFilter {
    /// Create a filter that matches everything.
    #[must_use]
    pub fn all() -> Self {
        Self {
            session_ids: None,
            recipients: None,
            kinds: None,
        }
    }

    /// Create a filter that matches messages addressed to a specific agent.
    #[must_use]
    pub fn for_agent(agent_id: AgentId) -> Self {
        Self {
            session_ids: None,
            recipients: Some(vec![
                Recipient::Agent(agent_id),
                Recipient::Broadcast,
            ]),
            kinds: None,
        }
    }

    /// Test whether a message matches this filter.
    #[must_use]
    pub fn matches(&self, msg: &BusMessage) -> bool {
        if let Some(ref ids) = self.session_ids {
            if !ids.contains(&msg.session_id) {
                return false;
            }
        }
        if let Some(ref recipients) = self.recipients {
            let ok = recipients.iter().any(|r| match r {
                Recipient::Broadcast => matches!(msg.recipient, Recipient::Broadcast),
                Recipient::Agent(id) => {
                    matches!(&msg.recipient, Recipient::Agent(rid) if rid == id)
                }
            });
            if !ok {
                return false;
            }
        }
        if let Some(ref kinds) = self.kinds {
            if !kinds.contains(&msg.kind) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// MessageBus trait
// ---------------------------------------------------------------------------

/// The message bus — publish/subscribe communication layer.
///
/// Implementations can range from in-process tokio channels to
/// distributed brokers (Redis, NATS, Kafka).
#[async_trait]
pub trait MessageBus: Send + Sync {
    /// Publish a message to all matching subscribers.
    ///
    /// The message is delivered to every subscriber whose
    /// [`SubscriptionFilter`] matches. If no subscriber matches, the
    /// message is silently dropped.
    async fn publish(&self, msg: BusMessage) -> Result<(), BusError>;

    /// Subscribe to messages matching a filter.
    ///
    /// Returns a receiver that yields matching messages as they are
    /// published. The receiver is closed when the bus is dropped.
    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
    ) -> Result<mpsc::UnboundedReceiver<BusMessage>, BusError>;
}

// ---------------------------------------------------------------------------
// BusError
// ---------------------------------------------------------------------------

/// Errors from bus operations.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    /// The publish operation failed (e.g., channel closed).
    #[error("failed to send message: {0}")]
    SendFailed(String),
    /// The subscribe operation failed.
    #[error("failed to subscribe: {0}")]
    SubscribeFailed(String),
}

// ---------------------------------------------------------------------------
// InProcessMessageBus
// ---------------------------------------------------------------------------

type SubscriptionId = Uuid;

struct Subscription {
    filter: SubscriptionFilter,
    sender: mpsc::UnboundedSender<BusMessage>,
}

/// An in-process message bus backed by tokio channels.
///
/// Suitable for single-process deployments. All subscribers are
/// notified synchronously within the `publish` call.
///
/// # Cloning
///
/// The bus is cheap to clone — all clones share the same subscription
/// table. Drop the last clone to shut down the bus.
#[derive(Clone, Default)]
pub struct InProcessMessageBus {
    subscriptions: Arc<RwLock<HashMap<SubscriptionId, Subscription>>>,
}

impl InProcessMessageBus {
    /// Create a new empty bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MessageBus for InProcessMessageBus {
    async fn publish(&self, msg: BusMessage) -> Result<(), BusError> {
        let subs = self.subscriptions.read().await;
        for sub in subs.values() {
            if sub.filter.matches(&msg) {
                // If the receiver is dropped, the send fails silently.
                let _ = sub.sender.send(msg.clone());
            }
        }
        Ok(())
    }

    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
    ) -> Result<mpsc::UnboundedReceiver<BusMessage>, BusError> {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = Uuid::new_v4();
        self.subscriptions
            .write()
            .await
            .insert(id, Subscription { filter, sender: tx });
        Ok(rx)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_msg(session: &str) -> BusMessage {
        BusMessage::user_chat(SessionId::new(session), "user-1", "hello")
    }

    #[tokio::test]
    async fn publish_subscribe_basic() {
        let bus = InProcessMessageBus::new();
        let mut rx = bus
            .subscribe(SubscriptionFilter::all())
            .await
            .expect("subscribe");

        bus.publish(test_msg("s1")).await.expect("publish");

        let received = rx.recv().await.expect("should receive message");
        assert_eq!(received.session_id, SessionId::new("s1"));
        assert_eq!(received.payload, "hello");
    }

    #[tokio::test]
    async fn filter_by_session() {
        let bus = InProcessMessageBus::new();
        let mut rx = bus
            .subscribe(SubscriptionFilter {
                session_ids: Some(vec![SessionId::new("s1")]),
                recipients: None,
                kinds: None,
            })
            .await
            .expect("subscribe");

        // Should match
        bus.publish(test_msg("s1")).await.expect("publish");
        assert!(rx.try_recv().is_ok());

        // Should NOT match
        bus.publish(test_msg("s2")).await.expect("publish");
        assert!(rx.try_recv().is_err()); // empty
    }

    #[tokio::test]
    async fn filter_by_recipient() {
        let bus = InProcessMessageBus::new();
        let agent_a = AgentId::new("agent-a");

        let mut rx = bus
            .subscribe(SubscriptionFilter::for_agent(agent_a.clone()))
            .await
            .expect("subscribe");

        // Directed to agent-a — should match
        let msg = BusMessage {
            recipient: Recipient::Agent(agent_a.clone()),
            ..test_msg("s1")
        };
        bus.publish(msg).await.expect("publish");
        assert!(rx.try_recv().is_ok());

        // Directed to agent-b — should NOT match
        let msg = BusMessage {
            recipient: Recipient::Agent(AgentId::new("agent-b")),
            ..test_msg("s1")
        };
        bus.publish(msg).await.expect("publish");
        assert!(rx.try_recv().is_err());

        // Broadcast — should match
        let msg = BusMessage {
            recipient: Recipient::Broadcast,
            ..test_msg("s1")
        };
        bus.publish(msg).await.expect("publish");
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn multiple_subscribers_all_receive() {
        let bus = InProcessMessageBus::new();
        let mut rx1 = bus
            .subscribe(SubscriptionFilter::all())
            .await
            .expect("sub1");
        let mut rx2 = bus
            .subscribe(SubscriptionFilter::all())
            .await
            .expect("sub2");

        bus.publish(test_msg("s1")).await.expect("publish");

        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    #[tokio::test]
    async fn clone_bus_shares_subscriptions() {
        let bus1 = InProcessMessageBus::new();
        let bus2 = bus1.clone();

        let mut rx = bus2
            .subscribe(SubscriptionFilter::all())
            .await
            .expect("subscribe");

        // Publish via bus1, receive via bus2's subscription
        bus1.publish(test_msg("s1")).await.expect("publish");
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn subscription_filter_for_agent_matches_correctly() {
        let agent_a = AgentId::new("agent-a");
        let filter = SubscriptionFilter::for_agent(agent_a.clone());

        // Match: directed to agent-a
        assert!(filter.matches(&BusMessage {
            recipient: Recipient::Agent(agent_a.clone()),
            ..test_msg("s1")
        }));

        // Match: broadcast
        assert!(filter.matches(&BusMessage {
            recipient: Recipient::Broadcast,
            ..test_msg("s1")
        }));

        // No match: directed to other agent
        assert!(!filter.matches(&BusMessage {
            recipient: Recipient::Agent(AgentId::new("agent-b")),
            ..test_msg("s1")
        }));
    }
}
