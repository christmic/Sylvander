//! Testbench orchestration check; detailed wire assertions remain provider-owned.

use std::fs;
use std::process::Command;

use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_executes_a_matrix_cell_through_the_production_adapter() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer explicit-child-key"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"sequence_number\":1,\"logprobs\":[],\"delta\":\"pong\"}\n\n",
                "data: {\"type\":\"response.completed\",\"response\":{",
                "\"id\":\"resp_1\",\"model\":\"model-a\",\"status\":\"completed\",",
                "\"output\":[{\"type\":\"message\",\"id\":\"msg_1\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"pong\",\"annotations\":[]}]}],",
                "\"usage\":{\"input_tokens\":7,\"output_tokens\":3,\"total_tokens\":10,",
                "\"input_tokens_details\":{\"cached_tokens\":0,\"cache_write_tokens\":0},",
                "\"output_tokens_details\":{\"reasoning_tokens\":0}}}}\n\n"
            ),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;
    let matrix_path = std::env::temp_dir().join(format!(
        "sylvander-llm-live-matrix-{}.json",
        std::process::id()
    ));
    let matrix = json!({
        "schema_version": 1,
        "repetitions": 1,
        "scenarios": ["usage"],
        "bindings": [{
            "provider_id": "provider-a",
            "protocol": "openai_responses",
            "base_url": server.uri(),
            "credential_env": "SYLVANDER_TESTBENCH_CHILD_KEY",
            "supported_scenarios": ["usage"],
            "models": [{
                "model_id": "model-a",
                "advertised_scenarios": ["usage"]
            }]
        }]
    });
    fs::write(&matrix_path, serde_json::to_vec(&matrix).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sylvander-llm-bench"))
        .args(["run", matrix_path.to_str().unwrap()])
        .env("SYLVANDER_TESTBENCH_CHILD_KEY", "explicit-child-key")
        .output()
        .unwrap();
    fs::remove_file(matrix_path).unwrap();

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(result["provider_id"], "provider-a");
    assert_eq!(result["protocol"], "openai_responses");
    assert_eq!(result["model_id"], "model-a");
    assert_eq!(result["scenario"], "usage");
    assert_eq!(result["input_tokens"], 7);
    assert_eq!(result["output_tokens"], 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_uses_the_protocol_specific_remote_token_count_operation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages/count_tokens"))
        .and(header("x-api-key", "explicit-anthropic-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 11})))
        .expect(1)
        .mount(&server)
        .await;
    let matrix_path = std::env::temp_dir().join(format!(
        "sylvander-llm-count-matrix-{}.json",
        std::process::id()
    ));
    let matrix = json!({
        "schema_version": 1,
        "repetitions": 1,
        "scenarios": ["remote_token_count"],
        "bindings": [{
            "provider_id": "anthropic-a",
            "protocol": "anthropic_messages",
            "base_url": server.uri(),
            "credential_env": "SYLVANDER_TESTBENCH_ANTHROPIC_CHILD_KEY",
            "supported_scenarios": ["remote_token_count"],
            "models": [{
                "model_id": "model-count-a",
                "advertised_scenarios": ["remote_token_count"]
            }]
        }]
    });
    fs::write(&matrix_path, serde_json::to_vec(&matrix).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sylvander-llm-bench"))
        .args(["run", matrix_path.to_str().unwrap()])
        .env(
            "SYLVANDER_TESTBENCH_ANTHROPIC_CHILD_KEY",
            "explicit-anthropic-key",
        )
        .output()
        .unwrap();
    fs::remove_file(matrix_path).unwrap();

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(result["scenario"], "remote_token_count");
    assert_eq!(result["counted_input_tokens"], 11);
    assert_eq!(result["input_tokens"], 0);
}
