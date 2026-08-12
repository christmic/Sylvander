//! Runtime-facing message bus port and bounded in-process adapter.
//!
//! The bus is an application transport contract, not a serializable API
//! shape. It lives beside Channel hosting so `sylvander-api` can remain a
//! pure wire-format and schema crate while Runtime still depends on a small,
//! replaceable delivery port.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock, mpsc};

use sylvander_api::{AgentId, BusMessage, MessageKind, Recipient, SessionId};

/// Observable bounded-delivery state for readiness and operations reporting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusDiagnostics {
    /// Whether publishers can be rejected instead of growing memory without bound.
    pub bounded: bool,
    /// Maximum queued messages retained for each subscriber.
    pub subscription_capacity: usize,
    /// Number of subscriptions currently registered with the adapter.
    pub subscriber_count: usize,
    /// Number of messages accepted for complete fan-out delivery.
    pub published_messages: u64,
    /// Number of publishes rejected before any subscriber received the message.
    pub backpressure_rejections: u64,
}

// ===========================================================================
// SubscriptionFilter
// ===========================================================================

/// Filter that determines which messages a subscriber receives.
#[derive(Debug, Clone)]
pub struct SubscriptionFilter {
    /// Optional allow-list of durable sessions.
    pub session_ids: Option<Vec<SessionId>>,
    /// Optional allow-list of direct or broadcast recipients.
    pub recipients: Option<Vec<Recipient>>,
    /// Optional allow-list of wire message kinds.
    pub kinds: Option<Vec<MessageKind>>,
}

impl SubscriptionFilter {
    /// Receive every message submitted to the bus.
    #[must_use]
    pub fn all() -> Self {
        Self {
            session_ids: None,
            recipients: None,
            kinds: None,
        }
    }

    /// Receive direct messages for one Agent plus broadcasts.
    #[must_use]
    pub fn for_agent(agent_id: AgentId) -> Self {
        Self {
            session_ids: None,
            recipients: Some(vec![Recipient::Agent(agent_id), Recipient::Broadcast]),
            kinds: None,
        }
    }

    /// Return whether a wire message satisfies every configured dimension.
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

/// Application port for bounded publication and filtered subscription.
///
/// Implementations must reject a publish before partial fan-out when any
/// matching live subscriber lacks capacity. This gives callers one observable
/// delivery result instead of silently producing divergent subscriber views.
#[async_trait]
pub trait MessageBus: Send + Sync {
    /// Publish a message to all matching subscribers.
    async fn publish(&self, msg: BusMessage) -> Result<(), BusError>;
    /// Subscribe to messages matching a filter.
    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
    ) -> Result<mpsc::Receiver<BusMessage>, BusError>;
    /// Report bounded-delivery counters for Runtime health aggregation.
    async fn diagnostics(&self) -> BusDiagnostics {
        BusDiagnostics::default()
    }
}

// ===========================================================================
// BusError
// ===========================================================================

/// Delivery failures produced by a [`MessageBus`] adapter.
#[derive(Debug, thiserror::Error)]
pub enum BusError {
    /// A live subscriber disappeared while accepting a message.
    #[error("failed to send message: {0}")]
    SendFailed(String),
    /// The adapter could not establish a subscription.
    #[error("failed to subscribe: {0}")]
    SubscribeFailed(String),
    /// At least one matching subscriber had no remaining bounded capacity.
    #[error("message bus is at capacity")]
    Backpressure,
}
type SubscriptionId = uuid::Uuid;

struct Subscription {
    filter: SubscriptionFilter,
    sender: mpsc::Sender<BusMessage>,
}

/// In-process message bus backed by tokio channels.
///
/// Suitable for single-process deployments.
#[derive(Clone)]
pub struct InProcessMessageBus {
    subscriptions: Arc<RwLock<HashMap<SubscriptionId, Subscription>>>,
    publish_lock: Arc<Mutex<()>>,
    subscription_capacity: usize,
    published_messages: Arc<AtomicU64>,
    backpressure_rejections: Arc<AtomicU64>,
}

impl InProcessMessageBus {
    const DEFAULT_SUBSCRIPTION_CAPACITY: usize = 256;

    /// Create a bounded in-process bus with the production default capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_SUBSCRIPTION_CAPACITY)
    }

    /// Create a bus with a fixed per-subscriber queue capacity.
    ///
    /// # Panics
    ///
    /// Panics when capacity is zero or above the operational hard limit.
    #[must_use]
    pub fn with_capacity(subscription_capacity: usize) -> Self {
        assert!(
            (1..=65_536).contains(&subscription_capacity),
            "message bus capacity must be between 1 and 65536"
        );
        Self {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            publish_lock: Arc::new(Mutex::new(())),
            subscription_capacity,
            published_messages: Arc::new(AtomicU64::new(0)),
            backpressure_rejections: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Default for InProcessMessageBus {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MessageBus for InProcessMessageBus {
    async fn publish(&self, msg: BusMessage) -> Result<(), BusError> {
        let _guard = self.publish_lock.lock().await;
        let mut subs = self.subscriptions.write().await;
        subs.retain(|_, subscription| !subscription.sender.is_closed());
        if subs
            .values()
            .any(|sub| sub.filter.matches(&msg) && sub.sender.capacity() == 0)
        {
            self.backpressure_rejections.fetch_add(1, Ordering::Relaxed);
            return Err(BusError::Backpressure);
        }
        for sub in subs.values() {
            if sub.filter.matches(&msg) {
                sub.sender
                    .try_send(msg.clone())
                    .map_err(|error| match error {
                        mpsc::error::TrySendError::Full(_) => BusError::Backpressure,
                        mpsc::error::TrySendError::Closed(_) => {
                            BusError::SendFailed("subscriber closed".into())
                        }
                    })?;
            }
        }
        self.published_messages.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
    ) -> Result<mpsc::Receiver<BusMessage>, BusError> {
        let (tx, rx) = mpsc::channel(self.subscription_capacity);
        let id = uuid::Uuid::new_v4();
        self.subscriptions
            .write()
            .await
            .insert(id, Subscription { filter, sender: tx });
        Ok(rx)
    }

    async fn diagnostics(&self) -> BusDiagnostics {
        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.retain(|_, subscription| !subscription.sender.is_closed());
        BusDiagnostics {
            bounded: true,
            subscription_capacity: self.subscription_capacity,
            subscriber_count: subscriptions.len(),
            published_messages: self.published_messages.load(Ordering::Relaxed),
            backpressure_rejections: self.backpressure_rejections.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/message_bus.rs"]
mod tests;
