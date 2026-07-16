//! In-process message bus implementation — Rust runtime only.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock, mpsc};

use crate::bus_trait::{BusError, MessageBus, SubscriptionFilter};
use crate::types::BusMessage;

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
}

impl InProcessMessageBus {
    const DEFAULT_SUBSCRIPTION_CAPACITY: usize = 256;

    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_SUBSCRIPTION_CAPACITY)
    }

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
        let subs = self.subscriptions.read().await;
        if subs.values().any(|sub| {
            sub.filter.matches(&msg) && !sub.sender.is_closed() && sub.sender.capacity() == 0
        }) {
            return Err(BusError::Backpressure);
        }
        for sub in subs.values() {
            if sub.filter.matches(&msg) && !sub.sender.is_closed() {
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
}

#[cfg(test)]
mod t {
    use super::*;
    use crate::bus_trait::SubscriptionFilter;
    use crate::types::{AgentId, BusMessage, Recipient, SessionId};
    fn tm(s: &str) -> BusMessage {
        BusMessage::user_chat(SessionId::new(s), "u1", "hi")
    }

    #[tokio::test]
    async fn pubsub() {
        let b = InProcessMessageBus::new();
        let mut r = b.subscribe(SubscriptionFilter::all()).await.unwrap();
        b.publish(tm("s1")).await.unwrap();
        assert!(r.try_recv().is_ok());
    }
    #[tokio::test]
    async fn filter_session() {
        let b = InProcessMessageBus::new();
        let mut r = b
            .subscribe(SubscriptionFilter {
                session_ids: Some(vec![SessionId::new("s1")]),
                recipients: None,
                kinds: None,
            })
            .await
            .unwrap();
        b.publish(tm("s1")).await.unwrap();
        assert!(r.try_recv().is_ok());
        b.publish(tm("s2")).await.unwrap();
        assert!(r.try_recv().is_err());
    }
    #[tokio::test]
    async fn filter_recipient() {
        let b = InProcessMessageBus::new();
        let a = AgentId::new("a");
        let mut r = b
            .subscribe(SubscriptionFilter::for_agent(a.clone()))
            .await
            .unwrap();
        b.publish(BusMessage {
            recipient: Recipient::Agent(a.clone()),
            ..tm("s1")
        })
        .await
        .unwrap();
        assert!(r.try_recv().is_ok());
        b.publish(BusMessage {
            recipient: Recipient::Agent(AgentId::new("b")),
            ..tm("s1")
        })
        .await
        .unwrap();
        assert!(r.try_recv().is_err());
        b.publish(BusMessage {
            recipient: Recipient::Broadcast,
            ..tm("s1")
        })
        .await
        .unwrap();
        assert!(r.try_recv().is_ok());
    }

    #[tokio::test]
    async fn saturated_subscriber_rejects_before_partial_delivery() {
        let b = InProcessMessageBus::with_capacity(1);
        let mut slow = b.subscribe(SubscriptionFilter::all()).await.unwrap();
        let mut other = b.subscribe(SubscriptionFilter::all()).await.unwrap();
        b.publish(tm("first")).await.unwrap();
        assert!(matches!(
            b.publish(tm("second")).await,
            Err(BusError::Backpressure)
        ));
        assert_eq!(
            other.recv().await.unwrap().session_id,
            SessionId::new("first")
        );
        assert!(other.try_recv().is_err());
        assert_eq!(
            slow.recv().await.unwrap().session_id,
            SessionId::new("first")
        );
    }
    #[test]
    fn filter_agent_matches() {
        let a = AgentId::new("a");
        let f = SubscriptionFilter::for_agent(a.clone());
        assert!(f.matches(&BusMessage {
            recipient: Recipient::Agent(a.clone()),
            ..tm("s1")
        }));
        assert!(f.matches(&BusMessage {
            recipient: Recipient::Broadcast,
            ..tm("s1")
        }));
        assert!(!f.matches(&BusMessage {
            recipient: Recipient::Agent(AgentId::new("b")),
            ..tm("s1")
        }));
    }
}
