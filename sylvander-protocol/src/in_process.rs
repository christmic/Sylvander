//! In-process message bus implementation — Rust runtime only.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock, mpsc};

use crate::bus_trait::{BusDiagnostics, BusError, MessageBus, SubscriptionFilter};
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
    published_messages: Arc<AtomicU64>,
    backpressure_rejections: Arc<AtomicU64>,
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
        let subs = self.subscriptions.read().await;
        if subs.values().any(|sub| {
            sub.filter.matches(&msg) && !sub.sender.is_closed() && sub.sender.capacity() == 0
        }) {
            self.backpressure_rejections.fetch_add(1, Ordering::Relaxed);
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
        BusDiagnostics {
            bounded: true,
            subscription_capacity: self.subscription_capacity,
            subscriber_count: self.subscriptions.read().await.len(),
            published_messages: self.published_messages.load(Ordering::Relaxed),
            backpressure_rejections: self.backpressure_rejections.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/in_process.rs"]
mod t;
