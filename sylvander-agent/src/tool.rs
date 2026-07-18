//! `Tool` trait + `ToolRegistry`.
//!
//! Tools are caller-pluggable. Production tools and runtime-owned dynamic
//! sources share this contract; test doubles live below `tests/`.
//!
//! The trait uses `async_trait` for dyn-compatibility + Send safety.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use sylvander_llm_anthropic::api::types::InputSchema;
use sylvander_protocol::AgentHookPhase;

use crate::tool_context::ToolContext;
use crate::tool_invocation::{ToolInvocationClass, ToolInvocationDescriptor};
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

    /// Security class used by the Runtime invocation gateway.
    ///
    /// New browser, host-control, terminal, and MCP adapters must override
    /// this method. Generic extensions remain isolated in their own class.
    fn invocation_class(&self) -> ToolInvocationClass {
        ToolInvocationClass::Extension
    }

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

/// One immutable hook command bound to an executable lifecycle phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolHookConfig {
    /// Stable, inspection-safe hook identity.
    pub name: String,
    /// Exact production boundary. No default is accepted.
    pub phase: AgentHookPhase,
    /// Operator-owned command; public inspection must redact this field.
    pub command: String,
    /// Per-invocation hard timeout, clamped again by the executor.
    #[serde(default = "default_hook_timeout_secs")]
    pub timeout_secs: u64,
    /// Whether failure stops or rejects the owning operation.
    #[serde(default)]
    pub blocking: bool,
}

const fn default_hook_timeout_secs() -> u64 {
    30
}

/// A blocking hook denied continuation at a named lifecycle boundary.
///
/// Commands and executor errors are deliberately absent from this public
/// error. Operators receive the phase and hook identity while hook output
/// remains on the bounded progress channel.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("blocking hook `{hook_name}` failed during `{phase}`")]
pub(crate) struct HookBlocked {
    hook_name: String,
    phase: &'static str,
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

    /// Replace the hook set for this immutable registry composition.
    ///
    /// Runtime installs changed hooks by composing and validating a new Agent
    /// revision before compare-and-swap activation. Existing sessions and
    /// frozen turns retain their prior capability revision; newly bound
    /// sessions receive the activated hook set without a server restart.
    #[must_use]
    pub fn with_hooks(mut self, hooks: Vec<ToolHookConfig>) -> Self {
        self.hooks = hooks;
        self
    }

    /// Execute a configured turn hook through the selected workspace executor.
    ///
    /// A before-turn hook runs exactly once before the first model iteration;
    /// an after-turn hook runs exactly once before a successful turn is
    /// published. Advisory failures are traced and do not change the turn.
    /// Blocking failures stop the turn with a content-safe [`HookBlocked`].
    pub(crate) async fn run_turn_hooks(
        &self,
        phase: AgentHookPhase,
        ctx: &ToolContext,
    ) -> Result<(), HookBlocked> {
        assert!(
            matches!(
                phase,
                AgentHookPhase::BeforeTurn | AgentHookPhase::AfterTurn
            ),
            "run_turn_hooks accepts only turn phases"
        );
        run_configured_hooks(&self.hooks, phase, ctx, ToolProgressSink::new(|_| {})).await
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
                        format!("{} · blocking", hook_phase_name(hook.phase))
                    } else {
                        format!("{} · advisory", hook_phase_name(hook.phase))
                    },
                    source: None,
                    trust: Some(sylvander_protocol::PlatformTrust::User),
                    auth: sylvander_protocol::PlatformAuthStatus::NotRequired,
                    capabilities: vec![hook_phase_name(hook.phase).into()],
                    // Hook changes are installed only through a validated Agent
                    // revision. Runtime re-composes that revision before CAS
                    // activation; frozen sessions keep their prior revision.
                    reloadable: true,
                }),
        );
        features
    }

    /// Exact descriptors used to freeze the Runtime authorization surface.
    #[must_use]
    pub fn invocation_descriptors(&self) -> Vec<ToolInvocationDescriptor> {
        let mut descriptors = self
            .snapshot()
            .into_values()
            .map(|tool| ToolInvocationDescriptor {
                name: tool.name().to_owned(),
                class: tool.invocation_class(),
                input_schema: tool.input_schema().schema,
            })
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| left.name.cmp(&right.name));
        descriptors
    }

    /// Return the content-addressed revision of the executable tool surface.
    ///
    /// The revision covers the current dynamic snapshot, schemas,
    /// descriptions, and lifecycle hooks. Persistent approvals bind to this
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
    /// Hooks are executable commands, so a restrictive clone must not retain
    /// them as an authority side channel.
    #[must_use]
    pub fn retain_named(&self, allowed: &[&str]) -> Self {
        Self {
            tools: self
                .unhooked_snapshot()
                .into_iter()
                .filter(|(name, _)| allowed.contains(&name.as_str()))
                .collect(),
            dynamic_sources: Vec::new(),
            hooks: Vec::new(),
        }
    }
}

const MAX_VISIBLE_HOOK_DELTA_CHARS: usize = 4_096;

const fn hook_phase_name(phase: AgentHookPhase) -> &'static str {
    match phase {
        AgentHookPhase::BeforeTool => "before_tool",
        AgentHookPhase::AfterTool => "after_tool",
        AgentHookPhase::BeforeTurn => "before_turn",
        AgentHookPhase::AfterTurn => "after_turn",
    }
}

fn bounded_hook_delta(delta: &str) -> String {
    let sanitized = delta
        .chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect::<String>();
    let mut chars = sanitized.chars();
    let visible = chars
        .by_ref()
        .take(MAX_VISIBLE_HOOK_DELTA_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{visible}\n… hook output delta truncated …\n")
    } else {
        visible
    }
}

async fn run_configured_hooks(
    hooks: &[ToolHookConfig],
    phase: AgentHookPhase,
    ctx: &ToolContext,
    progress: ToolProgressSink,
) -> Result<(), HookBlocked> {
    for hook in hooks.iter().filter(|hook| hook.phase == phase) {
        let phase_name = hook_phase_name(phase);
        progress.emit(format!("hook {} · {phase_name} · running\n", hook.name));
        let stdout_progress = progress.clone();
        let stderr_progress = progress.clone();
        let hook_progress = WorkspaceCommandProgressSink::new(move |stream, delta| match stream {
            WorkspaceCommandStream::Stdout => {
                stdout_progress.emit(format!("hook stdout · {}", bounded_hook_delta(&delta)));
            }
            WorkspaceCommandStream::Stderr => {
                stderr_progress.emit(format!("hook stderr · {}", bounded_hook_delta(&delta)));
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
                progress.emit(format!("hook {} · {phase_name} · passed\n", hook.name));
            }
            Ok(output) => {
                let decision = if hook.blocking { "blocked" } else { "failed" };
                progress.emit(format!(
                    "hook {} · {phase_name} · {decision} · exit {}\n",
                    hook.name,
                    output
                        .status_code
                        .map_or_else(|| "unknown".into(), |code| code.to_string())
                ));
                if hook.blocking {
                    return Err(HookBlocked {
                        hook_name: hook.name.clone(),
                        phase: phase_name,
                    });
                }
            }
            Err(error) => {
                let decision = if hook.blocking { "blocked" } else { "failed" };
                progress.emit(format!(
                    "hook {} · {phase_name} · {decision} · execution error\n",
                    hook.name
                ));
                tracing::warn!(
                    hook = %hook.name,
                    phase = phase_name,
                    %error,
                    "hook command execution failed"
                );
                if hook.blocking {
                    return Err(HookBlocked {
                        hook_name: hook.name.clone(),
                        phase: phase_name,
                    });
                }
            }
        }
    }
    Ok(())
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

    fn invocation_class(&self) -> ToolInvocationClass {
        self.inner.invocation_class()
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
        if let Err(blocked) = run_configured_hooks(
            &self.hooks,
            AgentHookPhase::BeforeTool,
            ctx,
            progress.clone(),
        )
        .await
        {
            return Ok(ToolOutput::err(format!(
                "{blocked}; tool `{}` was not executed",
                self.inner.name()
            )));
        }
        let result = self
            .inner
            .execute_streaming(ctx, input, progress.clone())
            .await;
        if let Err(blocked) =
            run_configured_hooks(&self.hooks, AgentHookPhase::AfterTool, ctx, progress).await
        {
            return match result {
                Ok(_) => Ok(ToolOutput::err(format!(
                    "{blocked}; tool `{}` result was rejected",
                    self.inner.name()
                ))),
                Err(_) => Err(ToolError::Other(blocked.to_string())),
            };
        }
        result
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

#[cfg(test)]
#[path = "../tests/unit/tool.rs"]
mod tests;
