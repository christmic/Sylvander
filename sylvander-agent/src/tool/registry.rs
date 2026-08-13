//! Tool registry, dynamic sources, hooks, and deferred discovery.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use sylvander_llm_core::{CacheHint, InputSchema, ToolDefinition as LlmToolDefinition};

use crate::execution::risk::{CommandRiskAssessment, CommandRiskLevel};
use crate::execution::tool_context::ToolContext;
use crate::execution::workspace::{WorkspaceCommandProgressSink, WorkspaceCommandStream};
use crate::tool::contract::{
    AgentHookPhase, PreparedToolCall, RegisteredTool, ToolDefinition, ToolError,
    ToolExecutionPolicy, ToolExecutor, ToolExposure, ToolOutput, ToolPreparation, ToolPrepareError,
    ToolProgressSink, ToolSourceFeature, ToolSourceKind, ToolSourceStatus, ToolSpec,
    validate_execution_environment,
};
#[cfg(test)]
use crate::tool::contract::{SandboxRequirement, ToolEnvironmentError, ToolExecutionMode};
use crate::tool::invocation::{ToolInvocationClass, ToolInvocationDescriptor};

const TOOL_SEARCH_NAME: &str = "tool_search";
const MAX_TOOL_SEARCH_MATCHES: usize = 8;
const MAX_TOOL_SEARCH_RESULT_BYTES: usize = 64 * 1024;

#[cfg(test)]
#[async_trait]
pub(crate) trait ToolTestExt {
    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError>;

    async fn execute_streaming(
        &self,
        ctx: &ToolContext,
        input: JsonValue,
        progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError>;
}

#[cfg(test)]
#[async_trait]
impl<T> ToolTestExt for T
where
    T: RegisteredTool + Clone + 'static,
{
    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        let name = self.spec().name;
        let call = ToolRegistry::new()
            .register(self.clone())
            .prepare(&name, input)
            .map_err(|error| ToolError::Other(error.to_string()))?;
        call.execute_streaming(ctx, ToolProgressSink::new(|_| {}))
            .await
    }

    async fn execute_streaming(
        &self,
        ctx: &ToolContext,
        input: JsonValue,
        progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        let name = self.spec().name;
        let call = ToolRegistry::new()
            .register(self.clone())
            .prepare(&name, input)
            .map_err(|error| ToolError::Other(error.to_string()))?;
        call.execute_streaming(ctx, progress).await
    }
}

/// A runtime-owned source whose tool catalog may change between turns.
///
/// Snapshots are synchronous and must be cheap. Transport work such as MCP
/// discovery happens before publishing a replacement snapshot.
pub trait DynamicToolSource: Send + Sync {
    fn snapshot(&self) -> Vec<Arc<dyn RegisteredTool>>;

    /// Optional redacted runtime state for UI inspection.
    fn platform_feature(&self) -> Option<ToolSourceFeature> {
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
    tools: HashMap<String, Arc<dyn RegisteredTool>>,
    dynamic_sources: Vec<Arc<dyn DynamicToolSource>>,
    hooks: Vec<ToolHookConfig>,
}

/// Failure to compose a Runtime-supplied Session tool surface with the
/// immutable Agent-revision registry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolRegistryCompositionError {
    /// Session extensions cannot replace an Agent-revision route.
    #[error("Session tool route `{0}` collides with the Agent tool surface")]
    DuplicateRoute(String),
    /// Lifecycle hooks belong to the Agent revision, not a Session extension.
    #[error("Session tool extensions cannot contribute lifecycle hooks")]
    ExtensionHooks,
}

impl ToolRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Consumes `self` for builder-style chaining.
    pub fn register<T: RegisteredTool + 'static>(mut self, tool: T) -> Self {
        let name = tool.spec().name;
        self.tools.insert(name, Arc::new(tool));
        self
    }

    /// Register a runtime-owned catalog that can atomically replace its tools.
    pub fn register_dynamic_source<S: DynamicToolSource + 'static>(mut self, source: S) -> Self {
        self.dynamic_sources.push(Arc::new(source));
        self
    }

    /// Freeze and compose a neutral Session extension without allowing it to
    /// replace revision-owned tools or hooks.
    ///
    /// Runtime uses this boundary for Session-scoped capability sources such
    /// as MCP. Transport and protocol types remain outside Agent; the result
    /// is an ordinary immutable registry suitable for one turn.
    pub fn compose_session_extensions(
        &self,
        extensions: &Self,
    ) -> Result<Self, ToolRegistryCompositionError> {
        if !extensions.hooks.is_empty() {
            return Err(ToolRegistryCompositionError::ExtensionHooks);
        }
        let (mut base, _) = self.freeze_for_turn();
        let (extensions, _) = extensions.freeze_for_turn();
        for (name, tool) in extensions.tools {
            if base.tools.insert(name.clone(), tool).is_some() {
                return Err(ToolRegistryCompositionError::DuplicateRoute(name));
            }
        }
        Ok(base)
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

    fn unhooked_snapshot(&self) -> HashMap<String, Arc<dyn RegisteredTool>> {
        let mut tools = self.tools.clone();
        for source in &self.dynamic_sources {
            for tool in source.snapshot() {
                tools.insert(tool.spec().name, tool);
            }
        }
        let deferred = tools
            .values()
            .filter(|tool| tool.spec().exposure == ToolExposure::Deferred)
            .cloned()
            .collect::<Vec<_>>();
        if !deferred.is_empty() {
            tools.insert(
                TOOL_SEARCH_NAME.into(),
                Arc::new(ToolSearchTool { deferred }) as Arc<dyn RegisteredTool>,
            );
        }
        tools
    }

    fn snapshot(&self) -> HashMap<String, Arc<dyn RegisteredTool>> {
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
                        }) as Arc<dyn RegisteredTool>,
                    )
                })
                .collect();
        }
        tools
    }

    /// Redacted runtime state contributed by dynamic capability sources.
    #[must_use]
    pub fn platform_features(&self) -> Vec<ToolSourceFeature> {
        let mut features = self
            .dynamic_sources
            .iter()
            .filter_map(|source| source.platform_feature())
            .collect::<Vec<_>>();
        features.extend(self.hooks.iter().map(|hook| ToolSourceFeature {
            kind: ToolSourceKind::Hook,
            name: hook.name.clone(),
            status: ToolSourceStatus::Configured,
            summary: if hook.blocking {
                format!("{} · blocking", hook_phase_name(hook.phase))
            } else {
                format!("{} · advisory", hook_phase_name(hook.phase))
            },
            source: None,
            requires_authentication: false,
            capabilities: vec![hook_phase_name(hook.phase).into()],
            // Hook changes are installed only through a validated Agent
            // revision. Runtime re-composes that revision before CAS
            // activation; frozen sessions keep their prior revision.
            reloadable: true,
        }));
        features
    }

    /// Exact descriptors used to freeze the Runtime authorization surface.
    #[must_use]
    pub fn invocation_descriptors(&self) -> Vec<ToolInvocationDescriptor> {
        let mut descriptors = self
            .snapshot()
            .into_values()
            .map(|tool| {
                let spec = tool.spec();
                ToolInvocationDescriptor {
                    name: spec.name,
                    class: spec.invocation_class,
                    input_schema: spec.input_schema,
                }
            })
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| left.name.cmp(&right.name));
        descriptors
    }

    /// Build deterministic prompt guidance for only the tools whose complete
    /// definitions are visible in this frozen turn.
    #[must_use]
    pub fn prompt_guidelines(&self) -> Option<String> {
        let mut tools = self
            .snapshot()
            .into_values()
            .map(|tool| tool.spec())
            .filter(|spec| {
                spec.exposure == ToolExposure::Immediate && !spec.prompt_guidelines.is_empty()
            })
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        let mut lines = Vec::new();
        for spec in tools {
            lines.extend(
                spec.prompt_guidelines
                    .into_iter()
                    .map(|guideline| format!("- [{}] {guideline}", spec.name)),
            );
        }
        (!lines.is_empty()).then(|| {
            format!(
                "Tool usage guidelines for the active tool set:\n{}",
                lines.join("\n")
            )
        })
    }

    /// Return the content-addressed revision of the executable tool surface.
    ///
    /// The revision covers the current dynamic snapshot, schemas,
    /// descriptions, and lifecycle hooks. Persistent approvals bind to this
    /// value so a catalog or hook change cannot reuse an older grant.
    #[must_use]
    pub fn capability_revision(&self) -> String {
        let mut surface = self
            .snapshot()
            .into_values()
            .map(|tool| {
                let spec = tool.spec();
                serde_json::json!({
                    "name": spec.name,
                    "description": spec.description,
                    "input_schema": spec.input_schema,
                    "class": invocation_class_name(spec.invocation_class),
                    "exposure": match spec.exposure {
                        ToolExposure::Immediate => "immediate",
                        ToolExposure::Deferred => "deferred",
                    },
                    "search_hint": spec.search_hint,
                    "prompt_guidelines": spec.prompt_guidelines,
                })
            })
            .collect::<Vec<_>>();
        surface.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
        let revision = serde_json::json!({
            "surface": surface,
            "hooks": self.hooks,
        });
        let mut hasher = Sha256::new();
        hasher.update(b"sylvander.tool.capability.v1\0");
        hasher.update(serde_json::to_vec(&revision).unwrap_or_default());
        format!("sha256:{:x}", hasher.finalize())
    }

    /// Freeze dynamic sources once for an immutable turn and return the exact
    /// revision to which approval grants must bind.
    /// Freeze dynamic sources at the Runtime turn boundary.
    #[must_use]
    pub fn freeze_for_turn(&self) -> (Self, String) {
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
    pub fn get(&self, name: &str) -> Option<Arc<dyn RegisteredTool>> {
        self.snapshot().remove(name)
    }

    /// Convert untrusted model input into one immutable executable call.
    pub fn prepare(
        &self,
        name: &str,
        input: JsonValue,
    ) -> Result<PreparedToolCall, ToolPrepareError> {
        let implementation = self
            .get(name)
            .ok_or_else(|| ToolPrepareError::Unavailable(name.to_owned()))?;
        let spec = Arc::new(implementation.spec());
        let preparation = implementation.prepare(input)?;
        let (input, execution_mode, execution_policy, command_risk) = preparation.into_parts();
        Ok(PreparedToolCall::new(
            implementation,
            spec,
            input,
            execution_mode,
            execution_policy,
            command_risk,
        ))
    }

    /// Iterate over all registered tools as `(name, &Arc<…>)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<dyn RegisteredTool>)> {
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

const fn invocation_class_name(class: ToolInvocationClass) -> &'static str {
    match class {
        ToolInvocationClass::Read => "read",
        ToolInvocationClass::FilesystemMutation => "filesystem_mutation",
        ToolInvocationClass::Terminal => "terminal",
        ToolInvocationClass::Browser => "browser",
        ToolInvocationClass::HostControl => "host_control",
        ToolInvocationClass::ArbitraryMcp => "arbitrary_mcp",
        ToolInvocationClass::MemoryCandidate => "memory_candidate",
        ToolInvocationClass::Control => "control",
        ToolInvocationClass::Extension => "extension",
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
        let risk = CommandRiskAssessment::evaluate(&hook.command);
        if risk.level != CommandRiskLevel::Routine {
            tracing::warn!(hook = %hook.name, phase = phase_name, risk = ?risk.level,
                "configured hook has non-routine command risk");
        }
        if let Err(error) = validate_execution_environment(
            &format!("hook:{}", hook.name),
            &ToolExecutionPolicy::process(),
            ctx,
        ) {
            tracing::warn!(hook = %hook.name, phase = phase_name, %error,
                "hook execution environment rejected");
            if hook.blocking {
                return Err(HookBlocked {
                    hook_name: hook.name.clone(),
                    phase: phase_name,
                });
            }
            continue;
        }
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
    inner: Arc<dyn RegisteredTool>,
    hooks: Vec<ToolHookConfig>,
}

impl ToolDefinition for HookedTool {
    fn spec(&self) -> ToolSpec {
        self.inner.spec()
    }

    fn prepare(&self, input: JsonValue) -> Result<ToolPreparation, ToolPrepareError> {
        self.inner.prepare(input)
    }
}

#[async_trait]
impl ToolExecutor for HookedTool {
    async fn handle(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        self.handle_streaming(ctx, call, ToolProgressSink::new(|_| {}))
            .await
    }

    async fn handle_streaming(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
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
                call.spec().name
            )));
        }
        let result = self
            .inner
            .handle_streaming(ctx, call, progress.clone())
            .await;
        if let Err(blocked) =
            run_configured_hooks(&self.hooks, AgentHookPhase::AfterTool, ctx, progress).await
        {
            return match result {
                Ok(_) => Ok(ToolOutput::err(format!(
                    "{blocked}; tool `{}` result was rejected",
                    call.spec().name
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
pub fn build_definitions(tools: &ToolRegistry) -> Vec<LlmToolDefinition> {
    let mut tools = tools
        .snapshot()
        .into_values()
        .map(|tool| tool.spec())
        .filter(|spec| spec.exposure == ToolExposure::Immediate)
        .collect::<Vec<_>>();
    tools.sort_by(|left, right| left.name.cmp(&right.name));
    let mut defs: Vec<_> = tools
        .into_iter()
        .map(|spec| LlmToolDefinition {
            name: spec.name,
            description: spec.description,
            input_schema: spec.input_schema,
            cache_hint: None,
        })
        .collect();
    if let Some(last) = defs.last_mut() {
        last.cache_hint = Some(CacheHint::Ephemeral);
    }
    defs
}

struct ToolSearchTool {
    deferred: Vec<Arc<dyn RegisteredTool>>,
}

impl ToolDefinition for ToolSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::immediate(
            TOOL_SEARCH_NAME,
            "Search deferred tools by capability, then call an exact returned tool name with its JSON schema.",
            InputSchema::from_json_value(serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 256,
                        "description": "Capability words to match against deferred tool names and descriptions"
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }))
            .schema,
            ToolInvocationClass::Read,
        )
    }
}

#[async_trait]
impl ToolExecutor for ToolSearchTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        let query = call
            .input()
            .get("query")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty() && query.len() <= 256)
            .ok_or_else(|| ToolError::Other("query must contain 1 to 256 bytes".into()))?;
        let terms = query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let mut candidates = self
            .deferred
            .iter()
            .filter(|tool| {
                let spec = tool.spec();
                let searchable = format!("{} {} {}", spec.name, spec.description, spec.search_hint)
                    .to_lowercase();
                terms.iter().all(|term| searchable.contains(term))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|tool| tool.spec().name);
        let total_matches = candidates.len();
        let mut matches = Vec::new();
        for tool in candidates.into_iter().take(MAX_TOOL_SEARCH_MATCHES) {
            let spec = tool.spec();
            let candidate = serde_json::json!({
                "name": spec.name,
                "description": spec.description,
                "input_schema": spec.input_schema,
            });
            let mut proposed = matches.clone();
            proposed.push(candidate.clone());
            let envelope = serde_json::json!({
                "matches": proposed,
                "total_matches": total_matches,
            });
            if serde_json::to_vec(&envelope)
                .is_ok_and(|bytes| bytes.len() <= MAX_TOOL_SEARCH_RESULT_BYTES)
            {
                matches.push(candidate);
            } else {
                break;
            }
        }
        Ok(ToolOutput::ok(
            serde_json::json!({
                "matches": matches,
                "total_matches": total_matches,
                "returned_matches": matches.len(),
            })
            .to_string(),
        ))
    }
}

impl ToolRegistry {
    /// Wire-format `Tool` definitions for the LLM request (with
    /// prompt caching on the last tool).
    #[must_use]
    pub fn definitions(&self) -> Vec<LlmToolDefinition> {
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
#[path = "../../tests/unit/tool.rs"]
mod tests;
