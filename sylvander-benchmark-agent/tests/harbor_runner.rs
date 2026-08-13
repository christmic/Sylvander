use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sylvander_benchmark_agent::harbor::{HarborRunConfig, run_harbor_task};
use sylvander_llm_core::{
    ContentBlock, ModelEventStream, ModelProvider, ModelRef, ModelRequest, ModelResponse,
    ModelStreamEvent, ProviderError, ProviderFuture, StopReason, TokenUsage, TokenUsageDetails,
};

struct ScriptedProvider {
    responses: Mutex<VecDeque<ModelResponse>>,
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
        },
    )
    .await;

    assert!(matches!(
        result,
        Err(sylvander_benchmark_agent::RecorderError::HarnessNotIsolated)
    ));
}
