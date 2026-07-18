use super::*;

#[test]
fn blocking_client_builds_with_default_config() {
    let client = BlockingAnthropicClient::new(AnthropicClient::builder().api_key("test-key"))
        .expect("blocking client should build");
    let _ = client.async_client();
}

#[test]
fn blocking_client_with_custom_config() {
    let client = BlockingAnthropicClient::with_config(
        AnthropicClient::builder().api_key("test-key"),
        BlockingConfig {
            enable_io: true,
            enable_time: true,
            thread_name: "test-thread".to_string(),
        },
    )
    .expect("blocking client should build");
    let _ = client.async_client();
}

#[test]
fn blocking_client_propagates_validation_errors() {
    // No API key set
    let result = BlockingAnthropicClient::new(AnthropicClient::builder());
    match result {
        Err(BlockingClientError::Client(AnthropicError::Validation(_))) => {}
        other => panic!("expected Client(Validation) error, got {other:?}"),
    }
}

#[test]
fn blocking_messages_api_exposes_async_api() {
    let client = BlockingAnthropicClient::new(AnthropicClient::builder().api_key("test-key"))
        .expect("build should succeed");
    let _api = client.messages();
}
