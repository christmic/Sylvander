use super::*;
use crate::tool_context::ToolContext;
use serde_json::json;

fn ctx() -> ToolContext {
    ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_capability(crate::tool_context::Cap::Read)
        .with_capability(crate::tool_context::Cap::Write)
        .with_capability(crate::tool_context::Cap::MemoryRead)
        .with_capability(crate::tool_context::Cap::MemoryWrite)
}

#[test]
fn tool_output_ok_constructor() {
    let out = ToolOutput::ok("file contents");
    assert!(!out.is_error);
    assert_eq!(out.content, "file contents");
}

#[test]
fn tool_output_err_constructor() {
    let out = ToolOutput::err("permission denied");
    assert!(out.is_error);
    assert_eq!(out.content, "permission denied");
}

#[test]
fn registry_register_and_get() {
    let tool = MockTool::new("echo", "echoes input", ToolOutput::ok("hi"));
    let registry = ToolRegistry::new().register(tool);
    assert_eq!(registry.len(), 1);
    assert!(!registry.is_empty());
    assert!(registry.get("echo").is_some());
    assert!(registry.get("missing").is_none());
}

#[test]
fn registry_iter_yields_names() {
    let registry = ToolRegistry::new()
        .register(MockTool::new("a", "first", ToolOutput::ok("a")))
        .register(MockTool::new("b", "second", ToolOutput::ok("b")));
    let names: Vec<&str> = registry.iter().map(|(name, _)| name).collect();
    assert!(names.contains(&"a"));
    assert!(names.contains(&"b"));
    assert_eq!(names.len(), 2);
}

#[test]
fn registry_definitions_for_llm() {
    let registry =
        ToolRegistry::new().register(MockTool::new("Read", "Read a file", ToolOutput::ok("")));
    let defs = registry.definitions();
    assert_eq!(defs.len(), 1);
    assert_eq!(defs[0].name, "Read");
    assert_eq!(defs[0].description, "Read a file");
}

#[tokio::test]
async fn mock_tool_records_calls() {
    let tool = MockTool::new("echo", "echo", ToolOutput::ok("hi"));
    let c = ctx();
    let _ = tool.execute(&c, json!({"input": "hello"})).await.unwrap();
    let _ = tool.execute(&c, json!({"input": "world"})).await.unwrap();
    let calls = tool.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["input"], "hello");
    assert_eq!(calls[1]["input"], "world");
    assert_eq!(tool.call_count(), 2);
}

#[tokio::test]
async fn mock_tool_cycles_responses() {
    let tool =
        MockTool::new("multi", "multiple responses", ToolOutput::ok("a")).with_responses(vec![
            ToolOutput::ok("first"),
            ToolOutput::ok("second"),
            ToolOutput::ok("third"),
        ]);
    let c = ctx();
    assert_eq!(tool.execute(&c, json!({})).await.unwrap().content, "first");
    assert_eq!(tool.execute(&c, json!({})).await.unwrap().content, "second");
    assert_eq!(tool.execute(&c, json!({})).await.unwrap().content, "third");
    // 4th call: cycles back to last configured response
    assert_eq!(tool.execute(&c, json!({})).await.unwrap().content, "third");
}

#[tokio::test]
async fn mock_tool_error_response() {
    let tool = MockTool::new("failing", "always fails", ToolOutput::err("boom"));
    let c = ctx();
    let out = tool.execute(&c, json!({})).await.unwrap();
    assert!(out.is_error);
    assert_eq!(out.content, "boom");
}

#[tokio::test]
async fn hooks_report_lifecycle_and_block_before_tool_execution() {
    let directory = tempfile::tempdir().unwrap();
    let inner = MockTool::new("write", "write", ToolOutput::ok("written"));
    let observed = inner.clone();
    let registry = ToolRegistry::new().register(inner).with_hooks(vec![
        ToolHookConfig {
            name: "lint".into(),
            command: "printf 'checked'".into(),
            timeout_secs: 5,
            blocking: false,
        },
        ToolHookConfig {
            name: "policy".into(),
            command: "exit 7".into(),
            timeout_secs: 5,
            blocking: true,
        },
    ]);
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let captured = deltas.clone();
    let output = registry
        .get("write")
        .unwrap()
        .execute_streaming(
            &ctx().with_fs_root(directory.path()),
            json!({"path":"file"}),
            ToolProgressSink::new(move |delta| captured.lock().unwrap().push(delta)),
        )
        .await
        .unwrap();

    assert!(output.is_error);
    assert!(output.content.contains("blocked by hook `policy`"));
    assert_eq!(observed.call_count(), 0);
    let lifecycle = deltas.lock().unwrap().join("");
    assert!(lifecycle.contains("hook lint · running"));
    assert!(lifecycle.contains("hook lint · passed"));
    assert!(lifecycle.contains("hook policy · blocked · exit 7"));
    let features = registry.platform_features();
    assert_eq!(features.len(), 2);
    assert_eq!(
        features[1].status,
        sylvander_protocol::PlatformFeatureStatus::Configured
    );
}

#[tokio::test]
async fn advisory_hook_failure_does_not_hide_the_tool_result() {
    let directory = tempfile::tempdir().unwrap();
    let inner = MockTool::new("read", "read", ToolOutput::ok("contents"));
    let observed = inner.clone();
    let registry = ToolRegistry::new()
        .register(inner)
        .with_hooks(vec![ToolHookConfig {
            name: "optional-check".into(),
            command: "exit 2".into(),
            timeout_secs: 5,
            blocking: false,
        }]);

    let output = registry
        .get("read")
        .unwrap()
        .execute(&ctx().with_fs_root(directory.path()), json!({}))
        .await
        .unwrap();
    assert_eq!(output, ToolOutput::ok("contents"));
    assert_eq!(observed.call_count(), 1);
}
