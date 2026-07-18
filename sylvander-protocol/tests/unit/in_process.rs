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
    assert_eq!(
        b.diagnostics().await,
        BusDiagnostics {
            bounded: true,
            subscription_capacity: 1,
            subscriber_count: 2,
            published_messages: 1,
            backpressure_rejections: 1,
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_publish_burst_delivers_every_message_within_capacity() {
    const PUBLISHERS: usize = 8;
    const MESSAGES_PER_PUBLISHER: usize = 500;
    const TOTAL: usize = PUBLISHERS * MESSAGES_PER_PUBLISHER;

    let bus = InProcessMessageBus::with_capacity(TOTAL);
    let mut receiver = bus.subscribe(SubscriptionFilter::all()).await.unwrap();
    let consumer = tokio::spawn(async move {
        let mut delivered = 0;
        while delivered < TOTAL {
            receiver.recv().await.expect("publisher remains alive");
            delivered += 1;
        }
        delivered
    });

    let mut publishers = Vec::new();
    for publisher in 0..PUBLISHERS {
        let bus = bus.clone();
        publishers.push(tokio::spawn(async move {
            for message in 0..MESSAGES_PER_PUBLISHER {
                bus.publish(tm(&format!("{publisher}-{message}")))
                    .await
                    .unwrap();
            }
        }));
    }
    for publisher in publishers {
        publisher.await.unwrap();
    }
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(5), consumer)
            .await
            .expect("consumer latency budget")
            .unwrap(),
        TOTAL
    );
    assert_eq!(bus.diagnostics().await.published_messages, TOTAL as u64);
    assert_eq!(bus.diagnostics().await.backpressure_rejections, 0);
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
