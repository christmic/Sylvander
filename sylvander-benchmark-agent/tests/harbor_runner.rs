use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sylvander_benchmark_agent::ProviderAudit;
use sylvander_benchmark_agent::harbor::{HarborRunConfig, run_harbor_task};
use sylvander_llm_core::{
    ContentBlock, ModelEventStream, ModelProvider, ModelRef, ModelRequest, ModelResponse,
    ModelStreamEvent, ProviderError, ProviderFuture, StopReason, TokenUsage, TokenUsageDetails,
};

struct ScriptedProvider {
    responses: Mutex<VecDeque<ModelResponse>>,
}

struct HangingAfterResponseProvider {
    response: Mutex<Option<ModelResponse>>,
}

impl ModelProvider for ScriptedProvider {
    fn complete_stream(&self, _request: ModelRequest) -> ProviderFuture<'_> {
        let response = self.responses.lock().unwrap().pop_front().unwrap();
        Box::pin(async move {
            let events: Vec<Result<ModelStreamEvent, ProviderError>> =
                vec![Ok(ModelStreamEvent::Completed(Box::new(response)))];
            Ok(Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
        })
    }
}

impl ModelProvider for HangingAfterResponseProvider {
    fn complete_stream(&self, _request: ModelRequest) -> ProviderFuture<'_> {
        let response = self.response.lock().unwrap().take();
        Box::pin(async move {
            if let Some(response) = response {
                let events: Vec<Result<ModelStreamEvent, ProviderError>> =
                    vec![Ok(ModelStreamEvent::Completed(Box::new(response)))];
                Ok(Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
            } else {
                std::future::pending().await
            }
        })
    }
}

fn response(content: Vec<ContentBlock>, stop_reason: StopReason) -> ModelResponse {
    ModelResponse {
        id: "response".into(),
        model: ModelRef::new("provider", "model"),
        content,
        stop_reason,
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            cache_write_tokens: None,
            cache_read_tokens: None,
            details: TokenUsageDetails::default(),
        },
    }
}

fn audit() -> ProviderAudit {
    ProviderAudit::new(
        "provider",
        "openai_chat_completions",
        "model",
        "https://provider.invalid/v1",
        "test-credential",
    )
}

#[tokio::test]
async fn executes_a_command_in_the_harness_workspace_and_records_trajectory() {
    let workspace = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(VecDeque::from([
            response(
                vec![ContentBlock::ToolCall {
                    id: "call-1".into(),
                    name: "Command".into(),
                    arguments: serde_json::json!({
                        "command": "printf solved > answer.txt"
                    }),
                }],
                StopReason::ToolUse,
            ),
            response(
                vec![ContentBlock::Text {
                    text: "task complete".into(),
                }],
                StopReason::EndTurn,
            ),
        ])),
    });

    let trajectory = run_harbor_task(
        provider,
        HarborRunConfig {
            session_id: "session".into(),
            provider_id: "provider".into(),
            model_id: "model".into(),
            workspace: workspace.path().into(),
            instruction: "create answer.txt".into(),
            max_iterations: 4,
            max_output_tokens: 128,
            timeout: Duration::from_secs(10),
            environment_isolated: true,
            trajectory_path: workspace.path().join("trajectory.json"),
            provider_audit: audit(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("answer.txt")).unwrap(),
        "solved"
    );
    assert_eq!(trajectory.steps.len(), 4);
    assert_eq!(trajectory.final_metrics.unwrap().total_prompt_tokens, 20);
}

#[tokio::test]
async fn rejects_execution_without_harness_isolation_attestation() {
    let workspace = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(VecDeque::new()),
    });
    let result = run_harbor_task(
        provider,
        HarborRunConfig {
            session_id: "session".into(),
            provider_id: "provider".into(),
            model_id: "model".into(),
            workspace: workspace.path().into(),
            instruction: "task".into(),
            max_iterations: 1,
            max_output_tokens: 16,
            timeout: Duration::from_secs(1),
            environment_isolated: false,
            trajectory_path: workspace.path().join("trajectory.json"),
            provider_audit: audit(),
        },
    )
    .await;

    assert!(matches!(
        result,
        Err(sylvander_benchmark_agent::RecorderError::HarnessNotIsolated)
    ));
}

#[tokio::test]
async fn command_timeout_terminates_the_complete_process_group() {
    let workspace = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider {
        responses: Mutex::new(VecDeque::from([
            response(
                vec![ContentBlock::ToolCall {
                    id: "call-timeout".into(),
                    name: "Command".into(),
                    arguments: serde_json::json!({
                        "command": "sleep 60 & echo $! > child.pid; wait"
                    }),
                }],
                StopReason::ToolUse,
            ),
            response(
                vec![ContentBlock::Text {
                    text: "timeout handled".into(),
                }],
                StopReason::EndTurn,
            ),
        ])),
    });

    let trajectory = run_harbor_task(
        provider,
        HarborRunConfig {
            session_id: "timeout-session".into(),
            provider_id: "provider".into(),
            model_id: "model".into(),
            workspace: workspace.path().into(),
            instruction: "exercise command timeout".into(),
            max_iterations: 4,
            max_output_tokens: 128,
            timeout: Duration::from_millis(100),
            environment_isolated: true,
            trajectory_path: workspace.path().join("trajectory.json"),
            provider_audit: audit(),
        },
    )
    .await
    .unwrap();

    let child_pid = std::fs::read_to_string(workspace.path().join("child.pid"))
        .unwrap()
        .trim()
        .to_owned();
    let mut child_is_alive = true;
    for _ in 0..20 {
        child_is_alive = std::process::Command::new("sh")
            .args([
                "-c",
                "kill -0 \"$1\" 2>/dev/null",
                "check-child",
                &child_pid,
            ])
            .status()
            .unwrap()
            .success();
        if !child_is_alive {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(
        !child_is_alive,
        "timed-out child process {child_pid} survived"
    );
    assert!(
        trajectory
            .steps
            .iter()
            .filter_map(|step| step.observation.as_ref())
            .flat_map(|observation| observation.results.iter())
            .filter_map(|result| result.content.as_deref())
            .any(|content| content.contains("timed out")),
        "trajectory did not retain the timeout: {:?}",
        trajectory.steps
    );
}

#[tokio::test]
async fn interrupted_run_retains_a_valid_content_safe_observability_checkpoint() {
    let workspace = tempfile::tempdir().unwrap();
    let credential = "credential-that-must-not-be-persisted";
    let trajectory_path = workspace.path().join("trajectory.json");
    let provider = Arc::new(HangingAfterResponseProvider {
        response: Mutex::new(Some(response(
            vec![ContentBlock::ToolCall {
                id: "call-observed".into(),
                name: "Command".into(),
                arguments: serde_json::json!({"command": "printf observed"}),
            }],
            StopReason::ToolUse,
        ))),
    });
    let run = run_harbor_task(
        provider,
        HarborRunConfig {
            session_id: "interrupted-session".into(),
            provider_id: "provider".into(),
            model_id: "model".into(),
            workspace: workspace.path().into(),
            instruction: "exercise interruption".into(),
            max_iterations: 4,
            max_output_tokens: 128,
            timeout: Duration::from_secs(10),
            environment_isolated: true,
            trajectory_path: trajectory_path.clone(),
            provider_audit: ProviderAudit::new(
                "provider",
                "openai_chat_completions",
                "model",
                "https://provider.invalid/v1",
                credential,
            ),
        },
    );

    assert!(
        tokio::time::timeout(Duration::from_millis(300), run)
            .await
            .is_err()
    );
    let encoded = std::fs::read_to_string(&trajectory_path).unwrap();
    assert!(!encoded.contains(credential));
    let trajectory: sylvander_benchmark_agent::Trajectory = serde_json::from_str(&encoded).unwrap();
    trajectory.validate().unwrap();
    let observability = &trajectory.extra.as_ref().unwrap()["sylvander_observability"];
    assert_eq!(observability["status"], "running");
    assert_eq!(
        observability["provider"]["credential_fingerprint"]
            .as_str()
            .unwrap()
            .len(),
        23
    );
    assert!(
        observability["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["kind"] == "iteration_finished" && event["response_id"] == "response"
            })
    );
    assert!(
        observability["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| {
                event["kind"] == "tool_finished" && event["call_id"] == "call-observed"
            })
    );
}
