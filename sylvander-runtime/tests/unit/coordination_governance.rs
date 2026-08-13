use super::*;

#[test]
fn strongly_connected_wait_components_are_hard_stops() {
    let task_id = TaskId::new("task");
    let waits = vec![
        WaitDependency {
            task_id: task_id.clone(),
            waiter: AgentInstanceId::new("a"),
            awaited: AgentInstanceId::new("b"),
        },
        WaitDependency {
            task_id: task_id.clone(),
            waiter: AgentInstanceId::new("b"),
            awaited: AgentInstanceId::new("c"),
        },
        WaitDependency {
            task_id,
            waiter: AgentInstanceId::new("c"),
            awaited: AgentInstanceId::new("a"),
        },
    ];

    assert_eq!(
        wait_cycles(&waits),
        [vec![
            AgentInstanceId::new("a"),
            AgentInstanceId::new("b"),
            AgentInstanceId::new("c"),
        ]]
    );
}

#[test]
fn unchanged_evidence_with_token_growth_requires_moderator_review() {
    let task_id = TaskId::new("task");
    let progress = (0..3)
        .map(|index| ProgressObservation {
            observation_id: format!("observation-{index}"),
            task_id: task_id.clone(),
            agent_instance_id: AgentInstanceId::new("worker"),
            task_revision: 2,
            consumed_tokens: 100 + index * 50,
            evidence_digest: Some("sha256:same".into()),
            observed_at: i64::try_from(index).unwrap(),
        })
        .collect::<Vec<_>>();
    let mut findings = Vec::new();

    assess_progress(&GovernancePolicy::default(), &progress, &mut findings);

    assert_eq!(
        findings,
        [GovernanceFinding::StagnantProgress {
            task_id,
            observations: 3,
        }]
    );
    assert_eq!(findings[0].severity(), FindingSeverity::ModeratorReview);
}

#[test]
fn alternating_handoffs_are_detected_as_ping_pong() {
    let task_id = TaskId::new("task");
    let handoffs = (0..4)
        .map(|index| {
            let (from, to) = if index % 2 == 0 {
                ("a", "b")
            } else {
                ("b", "a")
            };
            HandoffObservation {
                task_id: task_id.clone(),
                from: AgentInstanceId::new(from),
                to: AgentInstanceId::new(to),
                accepted_at: index,
            }
        })
        .collect::<Vec<_>>();
    let mut findings = Vec::new();

    assess_handoffs(&GovernancePolicy::default(), &handoffs, &mut findings);

    assert_eq!(
        findings,
        [GovernanceFinding::HandoffPingPong {
            task_id,
            handoffs: 4,
        }]
    );
}
