use std::collections::HashMap;
use std::fs;

use tempfile::TempDir;

use super::*;

const FAKE_SERVER: &str = r#"
import json
import os
import sys
import time

log_path = os.environ["MCP_TEST_LOG"]

def read_message():
    line = sys.stdin.readline()
    return json.loads(line) if line else None

def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method", "")
    with open(log_path, "a", encoding="utf-8") as log:
        log.write(method + "\n")
    if method == "initialize":
        send({"jsonrpc":"2.0", "id":message["id"], "result":{
            "protocolVersion":"2025-11-25",
            "capabilities":{"tools":{}, "resources":{}},
            "serverInfo":{"name":"fake", "version":"1"}
        }})
    elif method == "notifications/initialized":
        pass
    elif method == "ping":
        send({"jsonrpc":"2.0", "id":message["id"], "result":{}})
    elif method == "slow":
        time.sleep(0.3)
        send({"jsonrpc":"2.0", "id":message["id"], "result":{}})
    elif method == "tools/list":
        send({"jsonrpc":"2.0", "method":"notifications/tools/list_changed"})
        tool_name = "echo"
        if os.environ.get("MCP_TEST_DYNAMIC") == "1":
            with open(log_path, "r", encoding="utf-8") as log:
                if sum(1 for entry in log if entry.strip() == "tools/list") > 1:
                    tool_name = "echo_v2"
        send({"jsonrpc":"2.0", "id":message["id"], "result":{"tools":[{
            "name":tool_name,
            "description":"Echo an input value",
            "inputSchema":{"type":"object", "properties":{"value":{"type":"string"}}}
        }]}})
    elif method == "resources/list":
        send({"jsonrpc":"2.0", "id":message["id"], "result":{"resources":[{
            "uri":"memory://fake/guide",
            "name":"Fake guide",
            "mimeType":"text/markdown"
        }]}})
    elif method == "resources/read":
        uri = message.get("params", {}).get("uri", "")
        send({"jsonrpc":"2.0", "id":message["id"], "result":{"contents":[{
            "uri":uri,
            "mimeType":"text/markdown",
            "text":"Fake guide\nResource content"
        }]}})
    elif method == "tools/call":
        arguments = message.get("params", {}).get("arguments", {})
        if arguments.get("crash"):
            os._exit(3)
        if arguments.get("sleep"):
            time.sleep(0.3)
        send({"jsonrpc":"2.0", "id":message["id"], "result":{
            "content":[
                {"type":"text", "text":"echo:" + arguments.get("value", "")},
                {"type":"image", "mimeType":"image/png", "data":"AA=="}
            ],
            "isError":bool(arguments.get("error"))
        }})
"#;

fn fake_config(temp: &TempDir) -> McpServerConfig {
    let script = temp.path().join("fake_mcp_server.py");
    fs::write(&script, FAKE_SERVER).expect("write fake MCP server");
    let log = temp.path().join("requests.log");
    McpServerConfig {
        name: "fake".into(),
        command: "python3".into(),
        args: vec![script.display().to_string()],
        envs: HashMap::from([("MCP_TEST_LOG".into(), log.display().to_string())]),
    }
}

#[tokio::test]
async fn real_process_handshake_discovery_call_and_shutdown() {
    let temp = TempDir::new().expect("temp dir");
    let config = fake_config(&temp);
    let client = McpStdioClient::connect(&config, Duration::from_secs(2))
        .await
        .expect("connect");

    let tools = client.list_tools().await.expect("list tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "mcp__fake__echo");
    assert_eq!(tools[0].description(), "Echo an input value");
    assert_eq!(tools[0].input_schema().schema["type"], "object");
    let feature = DynamicToolSource::platform_feature(&client).expect("MCP health");
    assert_eq!(
        feature.status,
        sylvander_protocol::PlatformFeatureStatus::Active
    );
    assert!(feature.summary.contains("1 tools"));
    assert!(feature.summary.contains("1 resources"));
    assert!(feature.capabilities.contains(&"resources".to_owned()));
    client.probe_health().await.expect("health probe");

    let context = crate::tool_context::defaults::system_tool_context();
    let output = tools[0]
        .execute(&context, json!({ "value": "hello" }))
        .await
        .expect("call tool");
    assert!(!output.is_error);
    assert!(output.content.starts_with("echo:hello\n"));
    assert!(output.content.contains("\"type\":\"image\""));
    assert!(output.content.contains("<omitted 4 encoded bytes>"));
    assert!(!output.content.contains("AA=="));

    let model_error = tools[0]
        .execute(&context, json!({ "value": "no", "error": true }))
        .await
        .expect("model-visible tool error");
    assert!(model_error.is_error);
    assert!(model_error.content.starts_with("echo:no"));

    let resource_tools = client.resource_tools();
    assert_eq!(resource_tools.len(), 2);
    let list = resource_tools
        .iter()
        .find(|tool| tool.name().ends_with("__list_resources"))
        .expect("list resources tool");
    let listed = list
        .execute(&context, json!({}))
        .await
        .expect("list resources");
    assert!(listed.content.contains("memory://fake/guide"));
    let read = resource_tools
        .iter()
        .find(|tool| tool.name().ends_with("__read_resource"))
        .expect("read resource tool");
    let resource_output = read
        .execute(&context, json!({ "uri": "memory://fake/guide" }))
        .await
        .expect("read resource");
    assert!(resource_output.content.contains("Resource content"));

    client.shutdown().await.expect("shutdown process");
    let log = fs::read_to_string(temp.path().join("requests.log")).expect("read request log");
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "resources/list",
            "ping",
            "tools/call",
            "tools/call",
            "resources/list",
            "resources/read"
        ]
    );
}

#[tokio::test]
async fn tool_call_timeout_is_reported_and_process_can_be_stopped() {
    let temp = TempDir::new().expect("temp dir");
    let config = fake_config(&temp);
    let timeout = Duration::from_millis(200);
    let client = McpStdioClient::connect(&config, timeout)
        .await
        .expect("connect");
    let tool = client.list_tools().await.expect("list tools").remove(0);
    let context = crate::tool_context::defaults::system_tool_context();

    let error = tool
        .execute(&context, json!({ "sleep": true }))
        .await
        .expect_err("slow call must time out");
    assert!(matches!(error, ToolError::Timeout(duration) if duration == timeout));
    client.shutdown().await.expect("shutdown after timeout");
}

#[tokio::test]
async fn timeout_and_dropped_request_emit_protocol_cancellation() {
    let temp = TempDir::new().expect("temp dir");
    let config = fake_config(&temp);
    let client = McpStdioClient::connect(&config, Duration::from_millis(100))
        .await
        .expect("connect");

    let error = client
        .request("slow", json!({}))
        .await
        .expect_err("slow request must time out");
    assert!(matches!(error, McpError::Timeout { .. }));
    tokio::time::sleep(Duration::from_millis(300)).await;

    let interrupted_client = client.clone();
    let interrupted =
        tokio::spawn(async move { interrupted_client.request("slow", json!({})).await });
    tokio::time::sleep(Duration::from_millis(25)).await;
    interrupted.abort();
    assert!(interrupted.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(400)).await;

    let feature = DynamicToolSource::platform_feature(&client).expect("MCP health");
    assert!(feature.summary.contains("2 cancellations"));
    client.shutdown().await.expect("shutdown process");
    let log = fs::read_to_string(temp.path().join("requests.log")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|method| *method == "notifications/cancelled")
            .count(),
        2
    );
}

#[tokio::test]
async fn transport_failure_reconnects_for_the_next_tool_call_without_replaying_it() {
    let temp = TempDir::new().expect("temp dir");
    let config = fake_config(&temp);
    let client = McpStdioClient::connect(&config, Duration::from_secs(2))
        .await
        .expect("connect");
    let tool = client.list_tools().await.expect("list tools").remove(0);
    let context = crate::tool_context::defaults::system_tool_context();

    let error = tool
        .execute(&context, json!({ "crash": true }))
        .await
        .expect_err("crashed process must fail the in-flight call");
    assert!(matches!(error, ToolError::Other(_)));
    let recovered = tool
        .execute(&context, json!({ "value": "after-reconnect" }))
        .await
        .expect("the next call uses the replacement process");
    assert_eq!(
        recovered.content.lines().next(),
        Some("echo:after-reconnect")
    );

    client
        .shutdown()
        .await
        .expect("shutdown replacement process");
    let log = fs::read_to_string(temp.path().join("requests.log")).unwrap();
    assert_eq!(
        log.lines().filter(|method| *method == "initialize").count(),
        2
    );
    assert_eq!(
        log.lines().filter(|method| *method == "tools/call").count(),
        2
    );
}

#[tokio::test]
async fn reconnect_atomically_refreshes_the_dynamic_tool_catalog() {
    let temp = TempDir::new().expect("temp dir");
    let mut config = fake_config(&temp);
    config.envs.insert("MCP_TEST_DYNAMIC".into(), "1".into());
    let client = McpStdioClient::connect(&config, Duration::from_secs(2))
        .await
        .expect("connect");
    client.list_tools().await.expect("initial discovery");
    let registry = crate::tool::ToolRegistry::new().register_dynamic_source(client.clone());
    assert!(registry.get("mcp__fake__echo").is_some());
    assert!(registry.get("mcp__fake__echo_v2").is_none());

    let tool = registry.get("mcp__fake__echo").expect("initial tool");
    let context = crate::tool_context::defaults::system_tool_context();
    tool.execute(&context, json!({ "crash": true }))
        .await
        .expect_err("crashed call triggers reconnect");

    assert!(registry.get("mcp__fake__echo").is_none());
    assert!(registry.get("mcp__fake__echo_v2").is_some());
    let names = registry
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "mcp__fake__echo_v2",
            "mcp__fake__list_resources",
            "mcp__fake__read_resource"
        ]
    );
    let feature = DynamicToolSource::platform_feature(&client).expect("MCP health");
    assert_eq!(
        feature.status,
        sylvander_protocol::PlatformFeatureStatus::Active
    );
    assert!(feature.summary.contains("generation 2"));
    assert!(feature.summary.contains("1 reconnects"));

    client.shutdown().await.expect("shutdown replacement");
    assert_eq!(
        DynamicToolSource::platform_feature(&client)
            .expect("MCP health")
            .status,
        sylvander_protocol::PlatformFeatureStatus::Unavailable
    );
}

#[test]
fn tool_results_keep_unicode_safe_head_and_tail_with_explicit_truncation() {
    let content = format!("{}TAIL-蟹", "前".repeat(MAX_TOOL_RESULT_BYTES));
    let output = map_tool_result(
        &json!({
            "content": [{ "type": "text", "text": content }],
            "isError": false
        }),
        None,
    );

    assert!(output.content.len() <= MAX_TOOL_RESULT_BYTES);
    assert!(output.content.starts_with('前'));
    assert!(output.content.contains("MCP result truncated"));
    assert!(output.content.ends_with("TAIL-蟹"));
}

#[test]
fn public_tool_names_are_stable_bounded_and_mcp_namespaced() {
    assert_eq!(
        namespaced_tool_name("filesystem", "read_resource"),
        "mcp__filesystem__read_resource"
    );
    let transformed = namespaced_tool_name("本地 文件", "读取/资源");
    assert!(transformed.starts_with("mcp__"));
    assert!(transformed.len() <= 63);
    assert!(
        transformed
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    );
    assert_eq!(transformed, namespaced_tool_name("本地 文件", "读取/资源"));
    assert_ne!(
        namespaced_tool_name("server a", "read"),
        namespaced_tool_name("server/a", "read")
    );
}

#[tokio::test]
async fn complete_results_are_persisted_but_presented_as_bounded_summaries() {
    let directory = tempfile::tempdir().expect("tempdir");
    let result = json!({
        "content": [{
            "type": "text",
            "text": format!("{}TAIL", "x".repeat(MAX_TOOL_RESULT_BYTES))
        }],
        "structuredContent": {
            "kept": true
        }
    });

    let path = persist_result_artifact(
        directory.path(),
        "session/one",
        "search server",
        "lookup",
        &result,
    )
    .await
    .expect("persist result");
    let output = map_tool_result(&result, Some(&path));

    assert!(path.starts_with(directory.path().join("session_one/search_server")));
    assert_eq!(
        serde_json::from_slice::<JsonValue>(&tokio::fs::read(&path).await.unwrap()).unwrap(),
        result
    );
    assert!(output.content.contains("MCP result truncated"));
    assert!(output.content.contains("Full result artifact:"));
    assert!(output.content.len() <= MAX_TOOL_RESULT_BYTES);
}
