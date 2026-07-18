use super::*;
use crate::test_support::MockTool;
use crate::tool_context::ToolContext;
use serde_json::json;
use std::sync::{Arc, Mutex};

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
fn hook_progress_is_control_safe_and_bounded() {
    assert_eq!(bounded_hook_delta("ok\u{1b}[31m\n"), "ok\u{fffd}[31m\n");
    let oversized = "x".repeat(MAX_VISIBLE_HOOK_DELTA_CHARS + 1);
    let visible = bounded_hook_delta(&oversized);
    assert!(visible.contains("hook output delta truncated"));
    assert!(visible.len() < oversized.len() + 64);
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
fn restrictive_registry_clone_drops_executable_hooks() {
    let registry = ToolRegistry::new()
        .register(MockTool::new("read", "read", ToolOutput::ok("")))
        .register(MockTool::new("write", "write", ToolOutput::ok("")))
        .with_hooks(vec![ToolHookConfig {
            name: "side-channel".into(),
            phase: sylvander_protocol::AgentHookPhase::BeforeTurn,
            command: "touch escaped".into(),
            timeout_secs: 5,
            blocking: true,
        }]);

    let restricted = registry.retain_named(&["read"]);

    assert!(restricted.get("read").is_some());
    assert!(restricted.get("write").is_none());
    assert!(restricted.hooks.is_empty());
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

#[test]
fn capability_revision_tracks_tool_contract_and_hooks() {
    let base =
        ToolRegistry::new().register(MockTool::new("Read", "Read a file", ToolOutput::ok("")));
    let same =
        ToolRegistry::new().register(MockTool::new("Read", "Read a file", ToolOutput::ok("")));
    let changed_schema = ToolRegistry::new().register(MockTool::new(
        "Read",
        "Read a different contract",
        ToolOutput::ok(""),
    ));
    let hooked = base.clone().with_hooks(vec![ToolHookConfig {
        name: "policy".into(),
        phase: sylvander_protocol::AgentHookPhase::BeforeTool,
        command: "exit 0".into(),
        timeout_secs: 5,
        blocking: true,
    }]);
    let rephased = base.clone().with_hooks(vec![ToolHookConfig {
        name: "policy".into(),
        phase: sylvander_protocol::AgentHookPhase::AfterTool,
        command: "exit 0".into(),
        timeout_secs: 5,
        blocking: true,
    }]);

    assert_eq!(base.capability_revision(), same.capability_revision());
    assert_ne!(
        base.capability_revision(),
        changed_schema.capability_revision()
    );
    assert_ne!(base.capability_revision(), hooked.capability_revision());
    assert_ne!(
        hooked.capability_revision(),
        rephased.capability_revision(),
        "phase changes must bind approvals and revision reloads to new truth"
    );
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
            phase: sylvander_protocol::AgentHookPhase::BeforeTool,
            command: "printf 'checked'".into(),
            timeout_secs: 5,
            blocking: false,
        },
        ToolHookConfig {
            name: "policy".into(),
            phase: sylvander_protocol::AgentHookPhase::BeforeTool,
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
    assert!(
        output
            .content
            .contains("blocking hook `policy` failed during `before_tool`")
    );
    assert_eq!(observed.call_count(), 0);
    let lifecycle = deltas.lock().unwrap().join("");
    assert!(lifecycle.contains("hook lint · before_tool · running"));
    assert!(lifecycle.contains("hook lint · before_tool · passed"));
    assert!(lifecycle.contains("hook policy · before_tool · blocked · exit 7"));
    let features = registry.platform_features();
    assert_eq!(features.len(), 2);
    assert_eq!(
        features[1].status,
        sylvander_protocol::PlatformFeatureStatus::Configured
    );
    assert_eq!(features[1].capabilities, ["before_tool"]);
    assert!(features[1].reloadable);
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
            phase: sylvander_protocol::AgentHookPhase::BeforeTool,
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

#[tokio::test]
async fn blocking_after_tool_hook_rejects_an_already_executed_result() {
    let directory = tempfile::tempdir().unwrap();
    let inner = MockTool::new("read", "read", ToolOutput::ok("contents"));
    let observed = inner.clone();
    let registry = ToolRegistry::new()
        .register(inner)
        .with_hooks(vec![ToolHookConfig {
            name: "verify-result".into(),
            phase: sylvander_protocol::AgentHookPhase::AfterTool,
            command: "exit 9".into(),
            timeout_secs: 5,
            blocking: true,
        }]);

    let output = registry
        .get("read")
        .unwrap()
        .execute(&ctx().with_fs_root(directory.path()), json!({}))
        .await
        .unwrap();

    assert!(output.is_error);
    assert!(
        output
            .content
            .contains("blocking hook `verify-result` failed during `after_tool`")
    );
    assert_eq!(observed.call_count(), 1);
}

#[tokio::test]
async fn turn_hook_entry_runs_only_the_requested_phase() {
    let directory = tempfile::tempdir().unwrap();
    let registry = ToolRegistry::new().with_hooks(vec![
        ToolHookConfig {
            name: "before".into(),
            phase: sylvander_protocol::AgentHookPhase::BeforeTurn,
            command: "printf before > before-turn".into(),
            timeout_secs: 5,
            blocking: true,
        },
        ToolHookConfig {
            name: "after".into(),
            phase: sylvander_protocol::AgentHookPhase::AfterTurn,
            command: "printf after > after-turn".into(),
            timeout_secs: 5,
            blocking: true,
        },
    ]);
    let context = ctx().with_fs_root(directory.path());

    registry
        .run_turn_hooks(sylvander_protocol::AgentHookPhase::BeforeTurn, &context)
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(directory.path().join("before-turn")).unwrap(),
        "before"
    );
    assert!(!directory.path().join("after-turn").exists());
}
