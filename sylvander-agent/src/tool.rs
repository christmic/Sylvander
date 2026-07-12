//! `Tool` trait + `ToolRegistry`.
//!
//! Tools are caller-pluggable. M2 ships `MockTool` for tests; concrete
//! tools (Read / Bash / Edit / etc.) land in M3+ per the roadmap.
//!
//! The trait uses `async_trait` for dyn-compatibility + Send safety.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use thiserror::Error;

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool_context::ToolContext;

/// Output of a tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// Human-readable text content for the model. Becomes the `content`
    /// of a `tool_result` block.
    pub content: String,
    /// If `true`, the model sees this as a tool failure and can react
    /// accordingly. Distinct from [`ToolError`] (which is a system-level
    /// error that terminates the loop).
    pub is_error: bool,
}

impl ToolOutput {
    /// Successful tool output.
    #[must_use]
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    /// Error tool output — the model sees this as a failure.
    #[must_use]
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// System-level tool errors (panic, missing resource, etc.).
///
/// Distinct from [`ToolOutput::is_error`] — `is_error: true` is a
/// model-visible failure that flows through the loop; `ToolError`
/// terminates the loop.
#[derive(Debug, Error)]
pub enum ToolError {
    /// Tool execution panicked.
    #[error("tool panicked: {0}")]
    Panic(String),
    /// Tool exceeded its timeout.
    #[error("tool timed out after {0:?}")]
    Timeout(std::time::Duration),
    /// Other unrecoverable error.
    #[error("tool execution failed: {0}")]
    Other(String),
}

/// Trait implemented by all tools the agent can invoke.
///
/// `async_trait` provides dyn-compatibility + Send — needed so tools
/// can live in `Box<dyn Tool>` inside [`ToolRegistry`].
///
/// # Context
///
/// Every tool call receives a `&ToolContext` so implementations can
/// scope their work per-user / per-agent / per-session. Tools that
/// don't need isolation can ignore the `_ctx` argument; tools that
/// do (Read/Write/MemoryRead/MemoryWrite) use it to namespace data
/// and enforce permissions.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name — must match `Tool.name` in the wire `tools` array.
    fn name(&self) -> &str;

    /// Human-readable description — the model uses this to decide
    /// when to invoke the tool.
    fn description(&self) -> &str;

    /// JSON Schema describing the tool's input. Same as `Tool.input_schema`
    /// in the wire format.
    fn input_schema(&self) -> InputSchema;

    /// Execute the tool with the given input and invocation context.
    ///
    /// # Errors
    /// Returns [`ToolError`] for system-level failures (panic, timeout).
    /// For model-visible failures (e.g., "file not found"), return
    /// [`ToolOutput::err`] and let the loop continue.
    async fn execute(
        &self,
        ctx: &ToolContext,
        input: JsonValue,
    ) -> Result<ToolOutput, ToolError>;
}

/// Registry of tools available to the agent. Builder-style.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Consumes `self` for builder-style chaining.
    pub fn register<T: Tool + 'static>(mut self, tool: T) -> Self {
        let name = tool.name().to_string();
        self.tools.insert(name, Arc::new(tool));
        self
    }

    /// Number of registered tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// `true` if no tools are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Look up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Iterate over all registered tools as (name, &Arc<dyn Tool>) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<dyn Tool>)> {
        self.tools.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Clone a registry containing only explicitly allowed tool names.
    /// Used to give background work a smaller capability set than its parent.
    #[must_use]
    pub fn retain_named(&self, allowed: &[&str]) -> Self {
        Self {
            tools: self
                .tools
                .iter()
                .filter(|(name, _)| allowed.contains(&name.as_str()))
                .map(|(name, tool)| (name.clone(), tool.clone()))
                .collect(),
        }
    }
}

/// Wire-format `Tool` definitions for the LLM request, with prompt
/// caching enabled. The LAST tool in the array gets an
/// `ephemeral` `cache_control` breakpoint so the entire tools
/// block is cached across iterations.
pub fn build_definitions(tools: &ToolRegistry) -> Vec<sylvander_llm_anthropic::api::types::Tool> {
    let mut defs: Vec<_> = tools
        .iter()
        .map(|(_, t)| {
            sylvander_llm_anthropic::api::types::Tool::new(
                t.name(),
                t.description(),
                t.input_schema(),
            )
        })
        .collect();
    if let Some(last) = defs.last_mut() {
        use sylvander_llm_anthropic::api::types::CacheControl;
        last.cache_control = Some(CacheControl::ephemeral());
    }
    defs
}

impl ToolRegistry {
    /// Wire-format `Tool` definitions for the LLM request (with
    /// prompt caching on the last tool).
    #[must_use]
    pub fn definitions(&self) -> Vec<sylvander_llm_anthropic::api::types::Tool> {
        build_definitions(self)
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys().collect::<Vec<_>>())
            .finish()
    }
}

// =============================================================================
// MockTool — testing utility, not part of the public API.
// Available under #[cfg(test)] for unit tests in this crate, and re-exported
// for integration tests via `#[cfg(any(test, feature = "test-utils"))]` if needed.
// =============================================================================

/// In-memory mock tool. Records every call and returns a
/// pre-configured output. Used by integration tests to simulate
/// tool execution without spinning up real Read/Bash/etc.
///
/// **Note**: `MockTool` is only exposed under `#[cfg(test)]` in this
/// crate's unit tests. Integration tests in `tests/` use it directly
/// via the public `tool` module.
#[derive(Debug, Clone)]
pub struct MockTool {
    name: String,
    description: String,
    schema: InputSchema,
    responses: Vec<ToolOutput>,
    calls: Arc<Mutex<Vec<JsonValue>>>,
}

impl MockTool {
    /// Create a mock tool with the given name and a single canned
    /// response. Successive calls cycle through `responses` (last
    /// response repeats if exhausted).
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>, response: ToolOutput) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema: InputSchema::empty(),
            responses: vec![response],
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Set the input schema (defaults to empty object schema).
    #[must_use]
    pub fn with_schema(mut self, schema: InputSchema) -> Self {
        self.schema = schema;
        self
    }

    /// Provide multiple canned responses (cycled in order).
    #[must_use]
    pub fn with_responses(mut self, responses: Vec<ToolOutput>) -> Self {
        self.responses = responses;
        self
    }

    /// Get a snapshot of all calls made so far.
    #[must_use]
    pub fn calls(&self) -> Vec<JsonValue> {
        self.calls.lock().expect("MockTool lock poisoned").clone()
    }

    /// Number of calls made.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.lock().expect("MockTool lock poisoned").len()
    }
}

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> InputSchema {
        self.schema.clone()
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        input: JsonValue,
    ) -> Result<ToolOutput, ToolError> {
        self.calls
            .lock()
            .expect("MockTool lock poisoned")
            .push(input);
        // Cycle through responses
        let idx = self.calls.lock().expect("MockTool lock poisoned").len() - 1;
        let response = self
            .responses
            .get(idx)
            .or_else(|| self.responses.last())
            .cloned()
            .ok_or_else(|| ToolError::Other("no responses configured".into()))?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_context::ToolContext;
    use serde_json::json;

    fn ctx() -> ToolContext {
        ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s")).with_capability(crate::tool_context::Cap::Read).with_capability(crate::tool_context::Cap::Write).with_capability(crate::tool_context::Cap::MemoryRead).with_capability(crate::tool_context::Cap::MemoryWrite)
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
        let registry = ToolRegistry::new().register(MockTool::new(
            "Read",
            "Read a file",
            ToolOutput::ok(""),
        ));
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
        let tool = MockTool::new("multi", "multiple responses", ToolOutput::ok("a"))
            .with_responses(vec![
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
}
