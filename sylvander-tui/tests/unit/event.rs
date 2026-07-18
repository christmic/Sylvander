use super::*;

#[test]
fn backend_completion_events_are_agent_authoritative() {
    assert_eq!(
        DomainEvent::CompactionCompleted {
            report: sylvander_protocol::CompactionReport {
                automatic: false,
                removed_messages: 2,
                condensed_blocks: 1,
                freed_tokens: 5,
                summary: None,
            },
        }
        .source(),
        DomainEventSource::Agent
    );
    assert_eq!(
        DomainEvent::TaskCompleted {
            task_id: "task-1".into(),
            summary: "done".into(),
        }
        .source(),
        DomainEventSource::Agent
    );
    assert_eq!(DomainEvent::Tick.source(), DomainEventSource::RuntimeClock);
}
