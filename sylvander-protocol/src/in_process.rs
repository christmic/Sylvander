//! In-process message bus implementation — Rust runtime only.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, RwLock};

use crate::bus_trait::{BusError, MessageBus, SubscriptionFilter};
use crate::types::BusMessage;

type SubscriptionId = uuid::Uuid;

struct Subscription {
    filter: SubscriptionFilter,
    sender: mpsc::UnboundedSender<BusMessage>,
}

/// In-process message bus backed by tokio channels.
///
/// Suitable for single-process deployments.
#[derive(Clone, Default)]
pub struct InProcessMessageBus {
    subscriptions: Arc<RwLock<HashMap<SubscriptionId, Subscription>>>,
}

impl InProcessMessageBus {
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
    fn tm(s: &str) -> BusMessage { BusMessage::user_chat(SessionId::new(s), "u1", "hi") }

    #[tokio::test] async fn pubsub() {
        let b = InProcessMessageBus::new(); let mut r = b.subscribe(SubscriptionFilter::all()).await.unwrap();
        b.publish(tm("s1")).await.unwrap(); assert!(r.try_recv().is_ok());
    }
    #[tokio::test] async fn filter_session() {
        let b = InProcessMessageBus::new(); let mut r = b.subscribe(SubscriptionFilter{session_ids:Some(vec![SessionId::new("s1")]),recipients:None,kinds:None}).await.unwrap();
        b.publish(tm("s1")).await.unwrap(); assert!(r.try_recv().is_ok());
        b.publish(tm("s2")).await.unwrap(); assert!(r.try_recv().is_err());
    }
    #[tokio::test] async fn filter_recipient() {
        let b = InProcessMessageBus::new(); let a = AgentId::new("a");
        let mut r = b.subscribe(SubscriptionFilter::for_agent(a.clone())).await.unwrap();
        b.publish(BusMessage{recipient:Recipient::Agent(a.clone()),..tm("s1")}).await.unwrap(); assert!(r.try_recv().is_ok());
        b.publish(BusMessage{recipient:Recipient::Agent(AgentId::new("b")),..tm("s1")}).await.unwrap(); assert!(r.try_recv().is_err());
        b.publish(BusMessage{recipient:Recipient::Broadcast,..tm("s1")}).await.unwrap(); assert!(r.try_recv().is_ok());
    }
    #[test] fn filter_agent_matches() {
        let a = AgentId::new("a"); let f = SubscriptionFilter::for_agent(a.clone());
        assert!(f.matches(&BusMessage{recipient:Recipient::Agent(a.clone()),..tm("s1")}));
        assert!(f.matches(&BusMessage{recipient:Recipient::Broadcast,..tm("s1")}));
        assert!(!f.matches(&BusMessage{recipient:Recipient::Agent(AgentId::new("b")),..tm("s1")}));
    }
}
