use serde_json::json;
use sylvander_agent::conversation::ConversationSnapshot;
use sylvander_agent::error::AgentLoopError;
use sylvander_agent::event::AgentEvent;
use sylvander_agent::outcome::AgentOutcome;
use sylvander_benchmark_agent::TrajectoryRecorder;
use sylvander_llm_core::{
    ContentBlock, ModelRef, ModelResponse, StopReason, TokenUsage, TokenUsageDetails,
};

fn usage() -> TokenUsage {
    TokenUsage {
        input_tokens: 10,
        output_tokens: 5,
        cache_write_tokens: None,
        cache_read_tokens: Some(2),
        details: TokenUsageDetails::default(),
    }
}

#[test]
fn records_one_atif_step_per_model_iteration_with_correlated_tools() {
    let mut recorder = TrajectoryRecorder::new(
        "session-1",
        "minimax/MiniMax-M2.7",
        ["system policy".into()],
        "inspect",
    );
    recorder
        .record(AgentEvent::IterationStart { iteration: 1 })
        .unwrap();
    recorder
        .record(AgentEvent::ThinkingChunk("check files".into()))
        .unwrap();
    recorder
        .record(AgentEvent::ToolCallStart {
            id: "call-1".into(),
            name: "Command".into(),
            input: json!({"command": "ls"}),
        })
        .unwrap();
    recorder
        .record(AgentEvent::ToolCallEnd {
            id: "call-1".into(),
            name: "Command".into(),
            output: "Cargo.toml".into(),
            is_error: false,
        })
        .unwrap();
    recorder
        .record(AgentEvent::IterationEnd {
            iteration: 1,
            usage: usage(),
            provider_usage: usage(),
        })
        .unwrap();
    let response = ModelResponse {
        id: "response-1".into(),
        model: ModelRef::new("minimax", "MiniMax-M2.7"),
        content: vec![ContentBlock::Text {
            text: "done".into(),
        }],
        stop_reason: StopReason::EndTurn,
        usage: usage(),
    };
    recorder
        .record(AgentEvent::Done(AgentOutcome {
            final_response: response,
            conversation: ConversationSnapshot::default(),
            iterations: 1,
            total_usage: usage(),
        }))
        .unwrap();

    let trajectory = recorder.finish().unwrap();
    let agent_step = &trajectory.steps[2];
    assert_eq!(agent_step.llm_call_count, Some(1));
    assert_eq!(agent_step.metrics.unwrap().prompt_tokens, 12);
    assert_eq!(
        agent_step.tool_calls.as_ref().unwrap()[0].tool_call_id,
        "call-1"
    );
    assert_eq!(
        agent_step.observation.as_ref().unwrap().results[0]
            .source_call_id
            .as_deref(),
        Some("call-1")
    );
    assert_eq!(trajectory.final_metrics.unwrap().total_steps, 3);
}

#[test]
fn finish_rejects_an_incomplete_event_stream() {
    let recorder = TrajectoryRecorder::new("session-1", "provider/model", [], "task");
    assert!(recorder.finish().is_err());
}

#[test]
fn preserves_agent_terminal_error_detail() {
    let mut recorder = TrajectoryRecorder::new("session-1", "provider/model", [], "task");
    let error = recorder
        .record(AgentEvent::Error(AgentLoopError::Validation(
            "missing capability".into(),
        )))
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "Agent execution failed: validation error: missing capability"
    );
}
