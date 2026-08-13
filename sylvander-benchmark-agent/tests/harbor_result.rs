use sylvander_benchmark_agent::atif::{Agent, FinalMetrics, Source, Step, Trajectory};
use sylvander_benchmark_agent::harbor_result::{
    HarborAgentContext, HarborAgentInfo, HarborExceptionInfo, HarborModelInfo, HarborResultError,
    HarborTrialResult, HarborVerifierResult, normalize_harbor_result,
};
use sylvander_benchmark_agent::matrix::AgentMatrixCoordinate;
use sylvander_benchmark_agent::result::{AgentBenchStatus, RepositoryState};

fn coordinate() -> AgentMatrixCoordinate {
    AgentMatrixCoordinate {
        benchmark_id: "harbor".into(),
        dataset_name: "terminal-bench".into(),
        dataset_version: "2.0".into(),
        task_id: "task-a".into(),
        agent_revision: "revision".into(),
        provider_id: "provider".into(),
        protocol: "openai_chat_completions".into(),
        model_id: "model".into(),
        run_ordinal: 1,
    }
}

fn trial(reward: Option<f64>) -> HarborTrialResult {
    HarborTrialResult {
        task_name: "task-a".into(),
        agent_info: HarborAgentInfo {
            name: "sylvander".into(),
            model_info: Some(HarborModelInfo {
                name: "model".into(),
                provider: Some("provider".into()),
            }),
        },
        agent_result: Some(HarborAgentContext {
            n_input_tokens: Some(20),
            n_cache_tokens: Some(5),
            n_output_tokens: Some(10),
        }),
        verifier_result: reward.map(|reward| HarborVerifierResult {
            rewards: Some([("reward".into(), reward)].into()),
        }),
        exception_info: None,
        started_at: Some("2026-08-13T00:00:00Z".into()),
        finished_at: Some("2026-08-13T00:00:02.500Z".into()),
    }
}

fn trajectory() -> Trajectory {
    Trajectory {
        schema_version: "ATIF-v1.7".into(),
        session_id: Some("session".into()),
        trajectory_id: None,
        agent: Agent {
            name: "sylvander".into(),
            version: "0.1.0".into(),
            model_name: Some("provider/model".into()),
            tool_definitions: None,
        },
        steps: vec![Step {
            step_id: 1,
            source: Source::Agent,
            model_name: Some("provider/model".into()),
            message: "done".into(),
            reasoning_content: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            llm_call_count: Some(1),
        }],
        notes: None,
        final_metrics: Some(FinalMetrics {
            total_prompt_tokens: 20,
            total_completion_tokens: 10,
            total_cached_tokens: Some(5),
            total_steps: 1,
        }),
        extra: None,
    }
}

#[test]
fn normalizes_official_trial_reward_timing_and_atif_metrics() {
    let result = normalize_harbor_result(
        coordinate(),
        RepositoryState {
            sylvander_commit: "commit".into(),
            worktree_dirty: false,
        },
        "harbor-ea2fee7",
        &trial(Some(1.0)),
        &trajectory(),
    )
    .unwrap();
    assert_eq!(result.status, AgentBenchStatus::Passed);
    assert_eq!(result.reward, Some(1.0));
    assert_eq!(result.duration_ms, 2_500);
    assert_eq!(result.iterations, 1);
    assert_eq!(result.input_tokens, 20);
}

#[test]
fn records_missing_verifier_as_infrastructure_failure() {
    let mut value = trial(None);
    value.exception_info = Some(HarborExceptionInfo {
        exception_type: "RuntimeError".into(),
    });
    let result = normalize_harbor_result(
        coordinate(),
        RepositoryState {
            sylvander_commit: "commit".into(),
            worktree_dirty: false,
        },
        "harbor-ea2fee7",
        &value,
        &trajectory(),
    )
    .unwrap();
    assert_eq!(result.status, AgentBenchStatus::InfrastructureError);
    assert_eq!(result.failure_kind.as_deref(), Some("harbor_exception"));
}

#[test]
fn rejects_coordinate_or_metric_drift() {
    let mut wrong_task = trial(Some(1.0));
    wrong_task.task_name = "other".into();
    assert_eq!(
        normalize_harbor_result(
            coordinate(),
            RepositoryState {
                sylvander_commit: "commit".into(),
                worktree_dirty: false,
            },
            "harbor-ea2fee7",
            &wrong_task,
            &trajectory(),
        ),
        Err(HarborResultError::CoordinateMismatch)
    );

    let mut wrong_metrics = trial(Some(1.0));
    wrong_metrics.agent_result.as_mut().unwrap().n_input_tokens = Some(21);
    assert!(matches!(
        normalize_harbor_result(
            coordinate(),
            RepositoryState {
                sylvander_commit: "commit".into(),
                worktree_dirty: false,
            },
            "harbor-ea2fee7",
            &wrong_metrics,
            &trajectory(),
        ),
        Err(HarborResultError::MetricsMismatch)
    ));
}
