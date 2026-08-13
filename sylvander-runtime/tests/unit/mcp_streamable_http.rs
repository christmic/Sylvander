use std::sync::{Arc, Mutex};

use serde_json::{Value as JsonValue, json};
use sylvander_agent::tool::{DynamicToolSource, ToolExecutor as _};
use sylvander_agent::tool_context::defaults::system_tool_context;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;

#[derive(Clone, Default)]
struct ProtocolResponder {
    bearer_values: Arc<Mutex<Vec<Option<String>>>>,
}

impl Respond for ProtocolResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.bearer_values
            .lock()
            .expect("bearer observations")
            .push(
                request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            );
        let body: JsonValue = serde_json::from_slice(&request.body).expect("JSON-RPC request");
        let method = body
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if method == "notifications/initialized" {
            return ResponseTemplate::new(202);
        }
        let id = body.get("id").cloned().unwrap_or(JsonValue::Null);
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "remote", "version": "1"}
            }),
            "tools/list" => json!({"tools": [{
                "name": "echo",
                "description": "Echo a value",
                "inputSchema": {
                    "type": "object",
                    "properties": {"value": {"type": "string"}}
                }
            }]}),
            "tools/call" => json!({
                "content": [{"type": "text", "text": format!(
                    "echo:{}",
                    body["params"]["arguments"]["value"].as_str().unwrap_or_default()
                )}],
                "isError": false
            }),
            _ => json!({}),
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "application/json")
            .set_body_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }
}

fn config(url: String) -> McpStreamableHttpConfig {
    McpStreamableHttpConfig {
        name: "remote".into(),
        url,
        bearer_token: None,
    }
}

#[tokio::test]
async fn handshake_discovery_bearer_and_tool_call_use_one_endpoint() {
    let server = MockServer::start().await;
    let responder = ProtocolResponder::default();
    Mock::given(method("POST"))
        .respond_with(responder.clone())
        .mount(&server)
        .await;
    let client = McpStreamableHttpClient::connect(
        &config(format!("{}/mcp", server.uri())),
        Some("secret-token".into()),
        None,
    )
    .await
    .expect("connect Streamable HTTP MCP");

    let tools = client.snapshot();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spec().name, "mcp__remote__echo");
    let registry =
        sylvander_agent::tool::ToolRegistry::new().register_dynamic_source(client.clone());
    let call = registry
        .prepare("mcp__remote__echo", json!({"value": "hello"}))
        .expect("prepare remote call");
    let output = tools[0]
        .handle(&system_tool_context(), &call)
        .await
        .expect("execute remote tool");
    assert_eq!(output.content, "echo:hello");
    assert!(
        responder
            .bearer_values
            .lock()
            .expect("bearer observations")
            .iter()
            .all(|value| value.as_deref() == Some("Bearer secret-token"))
    );
    client.shutdown().await;
}

#[test]
fn endpoint_validation_rejects_credentials_and_plaintext_in_production_shape() {
    let embedded = config("https://user:secret@example.com/mcp".into());
    assert!(matches!(
        validate_endpoint(&embedded),
        Err(McpHttpError::InvalidEndpoint { .. })
    ));
    let invalid = config("not a URL".into());
    assert!(matches!(
        validate_endpoint(&invalid),
        Err(McpHttpError::InvalidEndpoint { .. })
    ));
}
