//! Production-entry tests for turn hook execution.

mod support;

use serde_json::json;
use sylvander_agent::prelude::{AgentLoopError, MessageParam, ToolContext, run};
use sylvander_agent::tool::{ToolHookConfig, ToolRegistry};
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use sylvander_protocol::{AgentHookPhase, SessionContext};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use support::qualified_anthropic_loop_builder;

fn client(server: &MockServer) -> AnthropicClient {
    AnthropicClient::builder()
        .api_key("test-key")
        .base_url(server.uri())
        .build()
        .expect("client build")
}

fn model() -> ModelInfo {
    ModelInfo::builder()
        .id("claude-sonnet-5-20260601")
        .context_window(200_000)
        .max_output_tokens(8_192)
        .capability(ModelCapabilities::TOOL_USE)
        .build()
        .expect("model build")
}

#[tokio::test]
async fn blocking_before_turn_hook_prevents_the_model_request() {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().expect("workspace");
    let hooks = ToolRegistry::new().with_hooks(vec![ToolHookConfig {
        name: "admission".into(),
        phase: AgentHookPhase::BeforeTurn,
        command: "exit 3".into(),
        timeout_secs: 5,
        blocking: true,
    }]);
    let loop_ = qualified_anthropic_loop_builder(client(&server), model())
        .tools(hooks)
        .tool_context(
            ToolContext::new(SessionContext::new("user", "agent", "session"))
                .with_fs_root(workspace.path()),
        )
        .build()
        .expect("loop build");

    let error = run(&loop_, vec![MessageParam::user("hello")])
        .await
        .expect_err("blocking hook must fail the turn");

    assert!(matches!(error, AgentLoopError::Tool(_)));
    assert!(
        error
            .to_string()
            .contains("blocking hook `admission` failed during `before_turn`")
    );
    assert!(
        server
            .received_requests()
            .await
            .expect("request log")
            .is_empty()
    );
}

#[tokio::test]
async fn successful_turn_runs_before_and_after_hooks_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_done",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "done"}],
            "model": "claude-sonnet-5-20260601",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 2}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().expect("workspace");
    let hooks = ToolRegistry::new().with_hooks(vec![
        ToolHookConfig {
            name: "open-turn".into(),
            phase: AgentHookPhase::BeforeTurn,
            command: "printf before >> lifecycle".into(),
            timeout_secs: 5,
            blocking: true,
        },
        ToolHookConfig {
            name: "close-turn".into(),
            phase: AgentHookPhase::AfterTurn,
            command: "printf after >> lifecycle".into(),
            timeout_secs: 5,
            blocking: true,
        },
    ]);
    let loop_ = qualified_anthropic_loop_builder(client(&server), model())
        .tools(hooks)
        .tool_context(
            ToolContext::new(SessionContext::new("user", "agent", "session"))
                .with_fs_root(workspace.path()),
        )
        .build()
        .expect("loop build");

    let result = run(&loop_, vec![MessageParam::user("hello")])
        .await
        .expect("turn succeeds");

    assert_eq!(result.iterations, 1);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("lifecycle")).expect("hook output"),
        "beforeafter"
    );
}
