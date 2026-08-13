use sylvander_benchmark_agent::matrix::AgentMatrixCoordinate;
use sylvander_benchmark_agent::result::{AgentBenchResult, AgentBenchStatus, RepositoryState};

fn coordinate() -> AgentMatrixCoordinate {
    AgentMatrixCoordinate {
        benchmark_id: "harbor".into(),
        dataset_name: "terminal-bench".into(),
        dataset_version: "2.0".into(),
        task_id: "task-a".into(),
        agent_revision: "agent-r1".into(),
        provider_id: "minimax".into(),
        protocol: "openai_chat_completions".into(),
        model_id: "MiniMax-M2.7".into(),
        run_ordinal: 1,
    }
}

#[test]
fn records_complete_content_safe_external_coordinate() {
    let result = AgentBenchResult::recorded(
        coordinate(),
        AgentBenchStatus::Passed,
        Some(1.0),
        RepositoryState {
            sylvander_commit: "commit".into(),
            worktree_dirty: false,
        },
        "harbor-ea2fee7",
        1_000,
        2,
        1,
        20,
        10,
        Some(5),
        None,
    )
    .unwrap();
    let value = serde_json::to_value(result).unwrap();

    assert_eq!(value["dataset_version"], "2.0");
    assert_eq!(value["reward"], 1.0);
    assert!(value.get("instruction").is_none());
    assert!(value.get("trajectory").is_none());
}

#[test]
fn rejects_missing_or_non_finite_verifier_rewards() {
    let repository = RepositoryState {
        sylvander_commit: "commit".into(),
        worktree_dirty: false,
    };
    assert!(
        AgentBenchResult::recorded(
            coordinate(),
            AgentBenchStatus::Failed,
            None,
            repository.clone(),
            "harbor-revision",
            0,
            0,
            0,
            0,
            0,
            None,
            None,
        )
        .is_err()
    );
    assert!(
        AgentBenchResult::recorded(
            coordinate(),
            AgentBenchStatus::Passed,
            Some(f64::NAN),
            repository,
            "harbor-revision",
            0,
            0,
            0,
            0,
            0,
            None,
            None,
        )
        .is_err()
    );
}
