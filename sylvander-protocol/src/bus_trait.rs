//! Message bus trait + subscription filter — Rust-only.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::types::{AgentId, BusMessage, MessageKind, Recipient, SessionId};

// ===========================================================================
// SubscriptionFilter
// ===========================================================================

/// Filter that determines which messages a subscriber receives.
#[derive(Debug, Clone)]
pub struct SubscriptionFilter {
    pub session_ids: Option<Vec<SessionId>>,
    pub recipients: Option<Vec<Recipient>>,
    pub kinds: Option<Vec<MessageKind>>,
}

impl SubscriptionFilter {
    #[must_use]
    pub fn all() -> Self {
        Self {
            session_ids: None,
            recipients: None,
            kinds: None,
        }
    }

    #[must_use]
    pub fn for_agent(agent_id: AgentId) -> Self {
        Self {
            session_ids: None,
            recipients: Some(vec![Recipient::Agent(agent_id), Recipient::Broadcast]),
            kinds: None,
        }
    }

    #[must_use]
    pub fn matches(&self, msg: &BusMessage) -> bool {
        if let Some(ref ids) = self.session_ids
            && !ids.contains(&msg.session_id)
        {
            return false;
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
        if let Some(ref kinds) = self.kinds
            && !kinds.contains(&msg.kind)
        {
            return false;
        }
        true
    }
}

// ===========================================================================
// MessageBus trait
// ===========================================================================

#[async_trait]
pub trait MessageBus: Send + Sync {
    /// Publish a message to all matching subscribers.
    async fn publish(&self, msg: BusMessage) -> Result<(), BusError>;
    /// Subscribe to messages matching a filter.
    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
    ) -> Result<mpsc::Receiver<BusMessage>, BusError>;
}

// ===========================================================================
// BusError
// ===========================================================================

#[derive(Debug, thiserror::Error)]
pub enum BusError {
    #[error("failed to send message: {0}")]
    SendFailed(String),
    #[error("failed to subscribe: {0}")]
    SubscribeFailed(String),
    #[error("message bus is at capacity")]
    Backpressure,
}
