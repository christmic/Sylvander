//! Official-derived native Generation tool-call chunk assembly.
//!
//! Evidence: dashscope-sdk-python `397e02b`, `samples/test_aio_generation.py`.

use futures_util::StreamExt as _;
use reqwest::Url;
use serde_json::json;
use sylvander_llm_core::{
    ContentBlock, ModelProvider, ModelRef, ModelRequest, ModelStreamEvent, StopReason,
    ToolDefinition,
};
use sylvander_llm_dashscope::{DashScopeFeatures, DashScopeProvider, DashScopeProviderConfig};
use wiremock::matchers::body_partial_json;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn qwen_tool_call_fragments_are_assembled_by_index() {
    let server = MockServer::start().await;
    let first = json!({
        "request_id": "req_tool",
        "output": {"choices": [{
            "index": 0,
            "message": {"content": "", "tool_calls": [{
                "index": 0,
                "id": "call_1",
                "function": {"name": "weather", "arguments": "{\"city\":\""}
            }]},
            "finish_reason": null
        }]},
        "usage": {"input_tokens": 5, "output_tokens": 2}
    });
    let second = json!({
        "request_id": "req_tool",
        "output": {"choices": [{
            "index": 0,
            "message": {"content": "", "tool_calls": [{
                "index": 0,
                "function": {"arguments": "杭州\"}"}
            }]},
            "finish_reason": "tool_calls"
        }]},
        "usage": {
            "input_tokens": 5,
            "output_tokens": 4,
            "total_tokens": 9,
            "prompt_tokens_details": {"cached_tokens": 1}
        }
    });
    Mock::given(body_partial_json(json!({
        "parameters": {
            "parallel_tool_calls": true,
            "tools": [{
                "type": "function",
                "function": {"name": "weather"}
            }]
        }
    })))
    .respond_with(ResponseTemplate::new(200).set_body_raw(
        format!("data: {first}\n\ndata: {second}\n\n"),
        "text/event-stream",
    ))
    .mount(&server)
    .await;
    let provider = DashScopeProvider::new(DashScopeProviderConfig {
        provider_id: "dashscope".into(),
        base_url: Url::parse(&server.uri()).expect("mock URL"),
        api_key: "key".into(),
        features: DashScopeFeatures::new(["parallel_tool_calls"]),
    })
    .expect("provider");
    let mut stream = provider
        .complete_stream(ModelRequest {
            request_id: "request".into(),
            model: ModelRef::new("dashscope", "qwen-plus"),
            system: Vec::new(),
            messages: vec![sylvander_llm_core::ChatMessage::user("weather")],
            tools: vec![ToolDefinition {
                name: "weather".into(),
                description: "Get weather".into(),
                input_schema: json!({"type": "object"}),
                cache_hint: None,
            }],
            max_output_tokens: 64,
            reasoning: None,
            output_schema: None,
        })
        .await
        .expect("open stream");
    let ModelStreamEvent::Completed(response) =
        stream.next().await.expect("terminal").expect("response")
    else {
        panic!("expected completion");
    };
    assert_eq!(response.stop_reason, StopReason::ToolUse);
    assert!(matches!(
        response.content.first(),
        Some(ContentBlock::ToolCall { name, arguments, .. })
            if name == "weather" && arguments == &json!({"city": "杭州"})
    ));
}
