use super::*;
use crate::execution_context::AgentExecutionContext;
use crate::test_support::MockTool;
use crate::tool_context::ToolContext;
use serde_json::json;
use std::sync::{Arc, Mutex};

struct DeferredWeatherTool;
struct ChangedDeferredWeatherTool;

impl ToolDefinition for DeferredWeatherTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = ToolSpec::immediate(
            "mcp_weather_forecast",
            "Get a weather forecast for one city",
            InputSchema::new_with_properties(json!({"city": {"type": "string"}}), &["city"]).schema,
            ToolInvocationClass::Extension,
        );
        spec.exposure = ToolExposure::Deferred;
        spec
    }
}

#[async_trait]
impl ToolExecutor for DeferredWeatherTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::ok("sunny"))
    }
}

impl ToolDefinition for ChangedDeferredWeatherTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = ToolSpec::immediate(
            "mcp_weather_forecast",
            "Get a detailed weather forecast for one city",
            InputSchema::new_with_properties(json!({"city": {"type": "string"}}), &["city"]).schema,
            ToolInvocationClass::Extension,
        );
        spec.exposure = ToolExposure::Deferred;
        spec
    }
}

#[async_trait]
impl ToolExecutor for ChangedDeferredWeatherTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::ok("sunny"))
    }
}

fn ctx() -> ToolContext {
    ToolContext::new(AgentExecutionContext::restricted_for("u", "a", "s"))
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
    let registry = ToolRegistry::new().register(tool.clone());
    assert_eq!(registry.len(), 1);
    assert!(!registry.is_empty());
    assert!(registry.get("echo").is_some());
    assert!(registry.get("missing").is_none());
}

#[test]
fn session_extensions_compose_without_replacing_revision_tools() {
    let base =
        ToolRegistry::new().register(MockTool::new("read", "base read", ToolOutput::ok("base")));
    let extension = ToolRegistry::new().register(MockTool::new(
        "mcp__search__query",
        "Session search",
        ToolOutput::ok("result"),
    ));

    let composed = base
        .compose_session_extensions(&extension)
        .expect("disjoint Session tools must compose");
    assert!(composed.get("read").is_some());
    assert!(composed.get("mcp__search__query").is_some());
}

#[test]
fn session_extensions_fail_closed_on_route_collision() {
    let base =
        ToolRegistry::new().register(MockTool::new("read", "base read", ToolOutput::ok("base")));
    let extension = ToolRegistry::new().register(MockTool::new(
        "read",
        "replacement",
        ToolOutput::ok("replacement"),
    ));

    assert_eq!(
        base.compose_session_extensions(&extension)
            .expect_err("Session extension must not replace a base route"),
        ToolRegistryCompositionError::DuplicateRoute("read".into())
    );
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
            phase: AgentHookPhase::BeforeTurn,
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

#[tokio::test]
async fn deferred_tools_are_searchable_without_eager_schema_exposure() {
    let registry = ToolRegistry::new().register(DeferredWeatherTool);
    let definitions = registry.definitions();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].name, TOOL_SEARCH_NAME);
    assert!(registry.get("mcp_weather_forecast").is_some());

    let search = registry
        .prepare(TOOL_SEARCH_NAME, json!({"query": "weather city"}))
        .expect("prepared search tool");
    let output = search
        .execute_streaming(&ctx(), ToolProgressSink::new(|_| {}))
        .await
        .expect("search execution");
    let result: JsonValue = serde_json::from_str(&output.content).expect("search JSON");
    assert_eq!(result["total_matches"], 1);
    assert_eq!(result["returned_matches"], 1);
    assert_eq!(result["matches"][0]["name"], "mcp_weather_forecast");
    assert_eq!(
        result["matches"][0]["input_schema"]["required"],
        json!(["city"])
    );
}

#[test]
fn deferred_contract_changes_invalidate_the_capability_revision() {
    let original = ToolRegistry::new().register(DeferredWeatherTool);
    let changed = ToolRegistry::new().register(ChangedDeferredWeatherTool);
    assert_ne!(
        original.capability_revision(),
        changed.capability_revision()
    );
}

#[test]
fn execution_mode_defaults_are_conservative_for_side_effects() {
    assert_eq!(
        ToolRegistry::new()
            .register(DeferredWeatherTool)
            .prepare("mcp_weather_forecast", json!({"city": "Hangzhou"}))
            .expect("prepared deferred tool")
            .execution_mode(),
        ToolExecutionMode::Parallel
    );
    assert_eq!(
        ToolRegistry::new()
            .register(crate::tools::WriteTool::new())
            .prepare("Write", json!({"file_path": "out", "content": "x"}))
            .expect("prepared write")
            .execution_mode(),
        ToolExecutionMode::Exclusive
    );
    assert_eq!(
        ToolRegistry::new()
            .register(crate::tools::CommandTool::new())
            .prepare("Command", json!({"command": "true"}))
            .expect("prepared command")
            .execution_mode(),
        ToolExecutionMode::Exclusive
    );
}

#[test]
fn process_tools_fail_closed_without_an_enforcing_sandbox() {
    let command = ToolRegistry::new()
        .register(crate::tools::CommandTool::new())
        .prepare("Command", json!({"command": "true"}))
        .expect("prepared command");
    let error = command
        .validate_environment(&ctx())
        .expect_err("local execution must not claim sandbox isolation");
    assert_eq!(
        error,
        ToolEnvironmentError::SandboxUnavailable("Command".into())
    );
}

#[test]
fn preparation_validates_declared_schema_before_authorization() {
    let commands = ToolRegistry::new().register(crate::tools::CommandTool::new());
    for input in [
        json!({}),
        json!({"command": 7}),
        json!({"command": "true", "environment": {"CI": false}}),
    ] {
        assert!(matches!(
            commands.prepare("Command", input),
            Err(ToolPrepareError::InvalidInput(_))
        ));
    }
    let reads = ToolRegistry::new().register(crate::tools::ReadTool::new());
    assert!(matches!(
        reads.prepare("Read", json!({"file_path": "README.md", "owner": "forged"})),
        Err(ToolPrepareError::InvalidInput(_))
    ));
}

#[test]
fn structured_workspace_tools_do_not_claim_process_sandboxing() {
    for (name, call) in [
        (
            "Read",
            ToolRegistry::new()
                .register(crate::tools::ReadTool::new())
                .prepare("Read", json!({"file_path": "README.md"}))
                .expect("prepared read"),
        ),
        (
            "Write",
            ToolRegistry::new()
                .register(crate::tools::WriteTool::new())
                .prepare("Write", json!({"file_path": "out", "content": "x"}))
                .expect("prepared write"),
        ),
    ] {
        assert_eq!(
            call.execution_policy().sandbox,
            SandboxRequirement::NotApplicable,
            "{name} is a structured executor operation"
        );
        call.validate_environment(&ctx())
            .expect("structured operation policy");
    }
}

#[test]
fn prepared_calls_are_identical_across_supported_model_families() {
    let registry = ToolRegistry::new().register(crate::tools::CommandTool::new());
    let input = json!({"command": "cargo test", "environment": {"CI": "1"}});
    let expected = registry
        .prepare("Command", input.clone())
        .expect("reference prepared call");
    for model in [
        sylvander_llm_core::ModelRef::new("anthropic", "claude-sonnet-5-20260601"),
        sylvander_llm_core::ModelRef::new("openai", "gpt-5.6"),
        sylvander_llm_core::ModelRef::new("dashscope", "qwen3-max"),
        sylvander_llm_core::ModelRef::new("deepseek", "deepseek-reasoner"),
    ] {
        let actual = registry
            .prepare("Command", input.clone())
            .expect("model-neutral prepared call");
        assert_eq!(actual.spec(), expected.spec(), "model={model:?}");
        assert_eq!(actual.input(), expected.input(), "model={model:?}");
        assert_eq!(
            actual.execution_policy(),
            expected.execution_policy(),
            "model={model:?}"
        );
        assert_eq!(
            actual.execution_mode(),
            expected.execution_mode(),
            "model={model:?}"
        );
    }
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
        phase: AgentHookPhase::BeforeTool,
        command: "exit 0".into(),
        timeout_secs: 5,
        blocking: true,
    }]);
    let rephased = base.clone().with_hooks(vec![ToolHookConfig {
        name: "policy".into(),
        phase: AgentHookPhase::AfterTool,
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
    let registry = ToolRegistry::new().register(tool.clone());
    let hello = registry.prepare("echo", json!({"input": "hello"})).unwrap();
    let world = registry.prepare("echo", json!({"input": "world"})).unwrap();
    let _ = hello
        .execute_streaming(&c, ToolProgressSink::new(|_| {}))
        .await
        .unwrap();
    let _ = world
        .execute_streaming(&c, ToolProgressSink::new(|_| {}))
        .await
        .unwrap();
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
    let registry = ToolRegistry::new().register(tool.clone());
    for expected in ["first", "second", "third"] {
        let call = registry.prepare("multi", json!({})).unwrap();
        let output = call
            .execute_streaming(&c, ToolProgressSink::new(|_| {}))
            .await
            .unwrap();
        assert_eq!(output.content, expected);
    }
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
            phase: AgentHookPhase::BeforeTool,
            command: "printf 'checked'".into(),
            timeout_secs: 5,
            blocking: false,
        },
        ToolHookConfig {
            name: "policy".into(),
            phase: AgentHookPhase::BeforeTool,
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
    assert_eq!(features[1].status, ToolSourceStatus::Configured);
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
            phase: AgentHookPhase::BeforeTool,
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
            phase: AgentHookPhase::AfterTool,
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
            phase: AgentHookPhase::BeforeTurn,
            command: "printf before > before-turn".into(),
            timeout_secs: 5,
            blocking: true,
        },
        ToolHookConfig {
            name: "after".into(),
            phase: AgentHookPhase::AfterTurn,
            command: "printf after > after-turn".into(),
            timeout_secs: 5,
            blocking: true,
        },
    ]);
    let context = ctx().with_fs_root(directory.path());

    registry
        .run_turn_hooks(AgentHookPhase::BeforeTurn, &context)
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(directory.path().join("before-turn")).unwrap(),
        "before"
    );
    assert!(!directory.path().join("after-turn").exists());
}
