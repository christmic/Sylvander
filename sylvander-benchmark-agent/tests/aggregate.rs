use sylvander_benchmark_agent::aggregate::{AggregateError, aggregate_results};
use sylvander_benchmark_agent::matrix::AgentMatrixCoordinate;
use sylvander_benchmark_agent::result::{AgentBenchResult, AgentBenchStatus, RepositoryState};

fn result(run_ordinal: u32, status: AgentBenchStatus, reward: Option<f64>) -> AgentBenchResult {
    AgentBenchResult::recorded(
        AgentMatrixCoordinate {
            benchmark_id: "harbor".into(),
            dataset_name: "terminal-bench".into(),
            dataset_version: "2.0".into(),
            task_id: "task-a".into(),
            agent_revision: "revision".into(),
            provider_id: "provider".into(),
            protocol: "openai_chat_completions".into(),
            model_id: "model".into(),
            run_ordinal,
        },
        status,
        reward,
        RepositoryState {
            sylvander_commit: "commit".into(),
            worktree_dirty: false,
        },
        "harbor-revision",
        100,
        1,
        2,
        10,
        5,
        Some(3),
        None,
    )
    .unwrap()
}

#[test]
fn aggregates_rewards_failures_cost_and_latency_per_deployment() {
    let aggregate = aggregate_results([
        result(1, AgentBenchStatus::Passed, Some(1.0)),
        result(2, AgentBenchStatus::Failed, Some(0.0)),
        result(3, AgentBenchStatus::InfrastructureError, None),
        result(4, AgentBenchStatus::AgentError, None),
    ])
    .unwrap()
    .remove(0);
    assert_eq!(aggregate.total_cells, 4);
    assert_eq!(aggregate.executed_cells, 2);
    assert_eq!(aggregate.infrastructure_errors, 1);
    assert_eq!(aggregate.agent_errors, 1);
    assert!((aggregate.mean_reward.unwrap() - 0.5).abs() < f64::EPSILON);
    assert!((aggregate.pass_rate.unwrap() - 0.5).abs() < f64::EPSILON);
    assert_eq!(aggregate.total_input_tokens, 40);
    assert_eq!(aggregate.total_cached_tokens, Some(12));
}

#[test]
fn rejects_duplicate_coordinates() {
    let value = result(1, AgentBenchStatus::Passed, Some(1.0));
    assert_eq!(
        aggregate_results([value.clone(), value]),
        Err(AggregateError::DuplicateCoordinate)
    );
}
