//! `Tool` trait + `ToolRegistry`.
//!
//! Tools are caller-pluggable. M2 ships `MockTool` for tests; concrete
//! tools (Read / Bash / Edit / etc.) land in M3+ per the roadmap.
//!
//! The trait uses `async_trait` for dyn-compatibility + Send safety.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool_context::ToolContext;
use crate::workspace_executor::{WorkspaceCommandProgressSink, WorkspaceCommandStream};

pub(crate) const TOOL_PROGRESS_CHANNEL_CAPACITY: usize = 64;
pub(crate) const TOOL_PROGRESS_OMITTED_MARKER: &str =
    "\n… intermediate tool output omitted because the progress buffer was full …\n";

/// Bounded interface for a tool to expose user-visible output while it runs.
/// The Agent owns transport and call identity; tools only emit text deltas.
#[derive(Clone)]
pub struct ToolProgressSink {
    emit_delta: Arc<dyn Fn(String) + Send + Sync>,
}

impl ToolProgressSink {
    pub(crate) fn new(emit_delta: impl Fn(String) + Send + Sync + 'static) -> Self {
        Self {
            emit_delta: Arc::new(emit_delta),
        }
    }

    pub(crate) fn bounded(
        try_emit: impl Fn(String) -> bool + Send + Sync + 'static,
    ) -> (Self, ToolProgressOmission) {
        let omitted = Arc::new(AtomicBool::new(false));
        let dropped = omitted.clone();
        let sink = Self::new(move |delta| {
            if !try_emit(delta) {
                dropped.store(true, Ordering::Release);
            }
        });
        (sink, ToolProgressOmission { omitted })
    }

    pub fn emit(&self, delta: impl Into<String>) {
        (self.emit_delta)(delta.into());
    }
}

pub(crate) struct ToolProgressOmission {
    omitted: Arc<AtomicBool>,
}

impl ToolProgressOmission {
    pub(crate) fn occurred(&self) -> bool {
        self.omitted.load(Ordering::Acquire)
    }
}

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
    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError>;

    /// Execute with optional incremental output. Existing tools remain source
    /// compatible; tools that can stream override this method and call
    /// `progress.emit(...)` without knowing anything about UI or transports.
    async fn execute_streaming(
        &self,
        ctx: &ToolContext,
        input: JsonValue,
        _progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        self.execute(ctx, input).await
    }
}

/// A runtime-owned source whose tool catalog may change between turns.
///
/// Snapshots are synchronous and must be cheap. Transport work such as MCP
/// discovery happens before publishing a replacement snapshot.
pub trait DynamicToolSource: Send + Sync {
    fn snapshot(&self) -> Vec<Arc<dyn Tool>>;

    /// Optional redacted runtime state for UI inspection.
    fn platform_feature(&self) -> Option<sylvander_protocol::PlatformFeature> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolHookConfig {
    pub name: String,
    pub command: String,
    #[serde(default = "default_hook_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub blocking: bool,
}

const fn default_hook_timeout_secs() -> u64 {
    30
}

/// Registry of tools available to the agent. Builder-style.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    dynamic_sources: Vec<Arc<dyn DynamicToolSource>>,
    hooks: Vec<ToolHookConfig>,
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

    /// Register a runtime-owned catalog that can atomically replace its tools.
    pub fn register_dynamic_source<S: DynamicToolSource + 'static>(mut self, source: S) -> Self {
        self.dynamic_sources.push(Arc::new(source));
        self
    }

    #[must_use]
    pub fn with_hooks(mut self, hooks: Vec<ToolHookConfig>) -> Self {
        self.hooks = hooks;
        self
    }

    fn unhooked_snapshot(&self) -> HashMap<String, Arc<dyn Tool>> {
        let mut tools = self.tools.clone();
        for source in &self.dynamic_sources {
            for tool in source.snapshot() {
                tools.insert(tool.name().to_string(), tool);
            }
        }
        tools
    }

    fn snapshot(&self) -> HashMap<String, Arc<dyn Tool>> {
        let mut tools = self.unhooked_snapshot();
        if !self.hooks.is_empty() {
            tools = tools
                .into_iter()
                .map(|(name, tool)| {
                    (
                        name,
                        Arc::new(HookedTool {
                            inner: tool,
                            hooks: self.hooks.clone(),
                        }) as Arc<dyn Tool>,
                    )
                })
                .collect();
        }
        tools
    }

    /// Redacted runtime state contributed by dynamic capability sources.
    #[must_use]
    pub fn platform_features(&self) -> Vec<sylvander_protocol::PlatformFeature> {
        let mut features = self
            .dynamic_sources
            .iter()
            .filter_map(|source| source.platform_feature())
            .collect::<Vec<_>>();
        features.extend(
            self.hooks
                .iter()
                .map(|hook| sylvander_protocol::PlatformFeature {
                    kind: sylvander_protocol::PlatformFeatureKind::Hook,
                    name: hook.name.clone(),
                    status: sylvander_protocol::PlatformFeatureStatus::Configured,
                    summary: if hook.blocking {
                        "before-tool · blocking".into()
                    } else {
                        "before-tool · advisory".into()
                    },
                    source: None,
                    trust: Some(sylvander_protocol::PlatformTrust::User),
                    auth: sylvander_protocol::PlatformAuthStatus::NotRequired,
                    capabilities: vec!["before_tool".into()],
                    reloadable: false,
                }),
        );
        features
    }

    /// Return the content-addressed revision of the executable tool surface.
    ///
    /// The revision covers the current dynamic snapshot, schemas,
    /// descriptions, and before-tool hooks. Persistent approvals bind to this
    /// value so a catalog or hook change cannot reuse an older grant.
    #[must_use]
    pub fn capability_revision(&self) -> String {
        let definitions = build_definitions(self);
        let revision = serde_json::json!({
            "definitions": definitions,
            "hooks": self.hooks,
        });
        let mut hasher = Sha256::new();
        hasher.update(b"sylvander.tool.capability.v1\0");
        hasher.update(serde_json::to_vec(&revision).unwrap_or_default());
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Freeze dynamic sources once for an immutable turn and return the exact
    /// revision to which approval grants must bind.
    pub(crate) fn freeze_with_revision(&self) -> (Self, String) {
        let frozen = Self {
            tools: self.unhooked_snapshot(),
            dynamic_sources: Vec::new(),
            hooks: self.hooks.clone(),
        };
        let revision = frozen.capability_revision();
        (frozen, revision)
    }

    /// Number of registered tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.snapshot().len()
    }

    /// `true` if no tools are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.snapshot().is_empty()
    }

    /// Look up a tool by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.snapshot().remove(name)
    }

    /// Iterate over all registered tools as `(name, &Arc<…>)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<dyn Tool>)> {
        self.tools.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Clone a registry containing only explicitly allowed tool names.
    /// Used to give background work a smaller capability set than its parent.
    #[must_use]
    pub fn retain_named(&self, allowed: &[&str]) -> Self {
        Self {
            tools: self
                .snapshot()
                .into_iter()
                .filter(|(name, _)| allowed.contains(&name.as_str()))
                .collect(),
            dynamic_sources: Vec::new(),
            hooks: Vec::new(),
        }
    }
}

struct HookedTool {
    inner: Arc<dyn Tool>,
    hooks: Vec<ToolHookConfig>,
}

#[async_trait]
impl Tool for HookedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn input_schema(&self) -> InputSchema {
        self.inner.input_schema()
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        self.execute_streaming(ctx, input, ToolProgressSink::new(|_| {}))
            .await
    }

    async fn execute_streaming(
        &self,
        ctx: &ToolContext,
        input: JsonValue,
        progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        for hook in &self.hooks {
            progress.emit(format!("hook {} · running\n", hook.name));
            let stdout_progress = progress.clone();
            let stderr_progress = progress.clone();
            let hook_progress =
                WorkspaceCommandProgressSink::new(move |stream, delta| match stream {
                    WorkspaceCommandStream::Stdout => {
                        stdout_progress.emit(format!("hook stdout · {delta}"));
                    }
                    WorkspaceCommandStream::Stderr => {
                        stderr_progress.emit(format!("hook stderr · {delta}"));
                    }
                });
            let result = ctx
                .executor
                .run_command_streaming(
                    &ctx.execution_target,
                    &hook.command,
                    Duration::from_secs(hook.timeout_secs.clamp(1, 300)),
                    hook_progress,
                )
                .await;
            match result {
                Ok(output) if output.success => {
                    progress.emit(format!("hook {} · passed\n", hook.name));
                }
                Ok(output) => {
                    let decision = if hook.blocking { "blocked" } else { "failed" };
                    progress.emit(format!(
                        "hook {} · {decision} · exit {}\n",
                        hook.name,
                        output
                            .status_code
                            .map_or_else(|| "unknown".into(), |code| code.to_string())
                    ));
                    if hook.blocking {
                        return Ok(ToolOutput::err(format!(
                            "blocked by hook `{}` before `{}`",
                            hook.name,
                            self.inner.name()
                        )));
                    }
                }
                Err(error) => {
                    let decision = if hook.blocking { "blocked" } else { "failed" };
                    progress.emit(format!("hook {} · {decision} · {error}\n", hook.name));
                    if hook.blocking {
                        return Ok(ToolOutput::err(format!(
                            "blocked by hook `{}` before `{}`",
                            hook.name,
                            self.inner.name()
                        )));
                    }
                }
            }
        }
        self.inner.execute_streaming(ctx, input, progress).await
    }
}

/// Wire-format `Tool` definitions for the LLM request, with prompt
/// caching enabled. The LAST tool in the array gets an
/// `ephemeral` `cache_control` breakpoint so the entire tools
/// block is cached across iterations.
pub fn build_definitions(tools: &ToolRegistry) -> Vec<sylvander_llm_anthropic::api::types::Tool> {
    let mut tools = tools.snapshot().into_values().collect::<Vec<_>>();
    tools.sort_by(|left, right| left.name().cmp(right.name()));
    let mut defs: Vec<_> = tools
        .into_iter()
        .map(|t| {
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
        let mut names = self.snapshot().into_keys().collect::<Vec<_>>();
        names.sort();
        f.debug_struct("ToolRegistry")
            .field("tools", &names)
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
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        response: ToolOutput,
    ) -> Self {
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

    async fn execute(&self, _ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
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
#[path = "../tests/unit/tool.rs"]
mod tests;
