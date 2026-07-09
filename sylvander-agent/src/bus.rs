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

/// Streaming events published during agent loop execution.
///
/// These are transient — they are NOT stored in session history.
/// Only [`StreamEvent::Done`] triggers a history write.
///
/// Named differently from `AgentEvent` to keep bus types independent
/// of agent internals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// Streaming text delta from the model.
    TextDelta {
        /// The text fragment.
        delta: String,
    },
    /// Streaming thinking delta (extended thinking).
    ThinkingDelta {
        /// The thinking fragment.
        delta: String,
    },
    /// The model invoked a tool. Published before execution.
    ToolCall {
        /// Tool call ID (matches the `tool_use.id`).
        call_id: String,
        /// Tool name.
        tool_name: String,
        /// Parsed input arguments (JSON).
        input: serde_json::Value,
    },
    /// Tool execution finished.
    ToolResult {
        /// Matching tool call ID.
        call_id: String,
        /// Tool name.
        tool_name: String,
        /// Tool output (success or error content).
        output: String,
        /// `true` if the tool returned an error.
        is_error: bool,
    },
    /// A new iteration is starting.
    IterationStart {
        /// 1-indexed iteration number.
        iteration: u32,
    },
    /// An iteration completed.
    IterationEnd {
        /// Iteration that just finished.
        iteration: u32,
        /// Input tokens consumed this iteration.
        input_tokens: u32,
        /// Output tokens produced this iteration.
        output_tokens: u32,
    },
    /// The loop completed successfully — final assembled text.
    Done {
        text: String,
    },

    /// Batch approval request — one or more tools need approval.
    ToolApprovalRequired {
        /// Unique batch identifier.
        batch_id: String,
        /// Tools waiting for approval.
        tools: Vec<ToolCallInfo>,
    },

    /// Model is asking the user a clarifying question. Execution is paused.
    AskUser {
        /// Tool call ID.
        call_id: String,
        /// The question.
        question: String,
        /// Available options (empty = free-text input).
        options: Vec<String>,
        /// If true, allow multiple selections.
        multi_select: bool,
    },

    /// User answered an AskUser question.
    UserAnswer {
        call_id: String,
        /// Selected options (1 for single, N for multi).
        answer: Vec<String>,
    },
}

/// Info about a single tool call — shared between approval requests
/// and execution events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallInfo {
    pub call_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
}

/// What kind of message this is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    /// A normal chat message (user ↔ agent).
    Chat,
    /// A system-level message (lifecycle, state, control).
    System(SystemMessage),
    /// A streaming event from an agent's loop execution.
    Stream(StreamEvent),
}

/// System-level messages for agent lifecycle and coordination.
///
/// These flow through the same bus as chat messages. Agents subscribe
/// to their own control channel; the engine subscribes to status updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemMessage {
    // -- Engine → Agent (control) --

    /// Stop the agent gracefully.
    Stop,

    /// Join a session — the agent should create a new SessionContext.
    JoinSession {
        /// The session to join.
        session_id: SessionId,
        /// Session metadata (workspace, name, user).
        metadata: crate::session::SessionMetadata,
    },

    /// Leave a session — the agent should drop its SessionContext.
    LeaveSession {
        /// The session to leave.
        session_id: SessionId,
    },

    // -- Agent → Engine (status) --

    /// Agent status update.
    StatusUpdate {
        status: AgentStatus,
    },

    /// Approve or reject a pending tool call (adapter → agent).
    ApproveTool {
        /// Tool call ID.
        call_id: String,
        /// `true` to execute, `false` to reject.
        approved: bool,
    },

    /// User answered an AskUser question (adapter → agent).
    AnswerQuestion {
        /// Tool call ID.
        call_id: String,
        /// User's selections (joined string).
        answer: String,
    },
}

/// Agent lifecycle status — published by the agent, observed by the engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    /// Agent task started, initializing.
    Starting,
    /// Agent is running its main loop.
    Running,
    /// Agent is idle (no pending messages).
    Idle,
    /// Agent has stopped.
    Stopped,
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

    // -- System message constructors --

    /// Tell an agent to stop.
    #[must_use]
    pub fn system_stop(agent_id: AgentId) -> Self {
        Self {
            session_id: SessionId::new(""), // agent-level, not session-scoped
            sender: Sender::System,
            recipient: Recipient::Agent(agent_id),
            kind: MessageKind::System(SystemMessage::Stop),
            payload: String::new(),
            timestamp: crate::session::now_secs(),
            id: MessageId::new(),
        }
    }

    /// Tell an agent to join a session.
    #[must_use]
    pub fn system_join_session(
        agent_id: AgentId,
        session_id: SessionId,
        metadata: crate::session::SessionMetadata,
    ) -> Self {
        Self {
            session_id: session_id.clone(),
            sender: Sender::System,
            recipient: Recipient::Agent(agent_id),
            kind: MessageKind::System(SystemMessage::JoinSession {
                session_id,
                metadata,
            }),
            payload: String::new(),
            timestamp: crate::session::now_secs(),
            id: MessageId::new(),
        }
    }

    /// Tell an agent to leave a session.
    #[must_use]
    pub fn system_leave_session(agent_id: AgentId, session_id: SessionId) -> Self {
        Self {
            session_id: session_id.clone(),
            sender: Sender::System,
            recipient: Recipient::Agent(agent_id),
            kind: MessageKind::System(SystemMessage::LeaveSession { session_id }),
            payload: String::new(),
            timestamp: crate::session::now_secs(),
            id: MessageId::new(),
        }
    }

    /// Publish an agent's status update.
    #[must_use]
    pub fn system_status_update(agent_id: AgentId, status: AgentStatus) -> Self {
        Self {
            session_id: SessionId::new(""), // agent-level
            sender: Sender::Agent(agent_id),
            recipient: Recipient::Broadcast,
            kind: MessageKind::System(SystemMessage::StatusUpdate { status }),
            payload: String::new(),
            timestamp: crate::session::now_secs(),
            id: MessageId::new(),
        }
    }

    // -- Stream constructors --

    /// Create a streaming event message.
    #[must_use]
    pub fn stream_event(
        session_id: SessionId,
        agent_id: AgentId,
        event: StreamEvent,
    ) -> Self {
        Self {
            session_id,
            sender: Sender::Agent(agent_id),
            recipient: Recipient::Broadcast,
            kind: MessageKind::Stream(event),
            payload: String::new(),
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
