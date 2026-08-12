//! Tool definition, preparation, execution, and registration contracts.
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

use sylvander_llm_core::{CacheHint, InputSchema, ToolDefinition as LlmToolDefinition};
use sylvander_protocol::AgentHookPhase;

use crate::tool_context::ToolContext;
use crate::tool_invocation::{ToolInvocationClass, ToolInvocationDescriptor};
use crate::workspace_executor::{WorkspaceCommandProgressSink, WorkspaceCommandStream};

pub(crate) const TOOL_PROGRESS_CHANNEL_CAPACITY: usize = 64;
pub(crate) const TOOL_PROGRESS_OMITTED_MARKER: &str =
    "\n… intermediate tool output omitted because the progress buffer was full …\n";
const TOOL_SEARCH_NAME: &str = "tool_search";
const MAX_TOOL_SEARCH_MATCHES: usize = 8;
const MAX_TOOL_SEARCH_RESULT_BYTES: usize = 64 * 1024;

/// Whether a tool schema is sent on every model request or discovered on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExposure {
    /// Include the complete schema in the ordinary model tool list.
    Immediate,
    /// Keep the executable route authorized but expose its schema through
    /// `tool_search` only.
    Deferred,
}

/// Coordination required when multiple tool calls occur in one model batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolExecutionMode {
    /// The call may overlap other parallel calls from the same batch.
    Parallel,
    /// The call must not overlap any other executable call from the batch.
    Exclusive,
}

/// Whether OS policy must constrain one prepared invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxRequirement {
    /// The call executes entirely inside the trusted Agent process.
    NotApplicable,
    /// Apply a sandbox when the selected execution environment supports it.
    Preferred,
    /// Fail closed unless the selected environment can enforce the policy.
    Required,
}

/// Filesystem authority requested by trusted tool preparation code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFilesystemPolicy {
    None,
    WorkspaceRead,
    WorkspaceWrite,
}

/// Network authority requested by trusted tool preparation code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolNetworkPolicy {
    Denied,
    FullAfterApproval,
}

/// Execution-environment requirements frozen before authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionPolicy {
    pub sandbox: SandboxRequirement,
    pub filesystem: ToolFilesystemPolicy,
    pub network: ToolNetworkPolicy,
    pub launches_processes: bool,
}

impl ToolExecutionPolicy {
    #[must_use]
    pub const fn in_process() -> Self {
        Self {
            sandbox: SandboxRequirement::NotApplicable,
            filesystem: ToolFilesystemPolicy::None,
            network: ToolNetworkPolicy::Denied,
            launches_processes: false,
        }
    }

    #[must_use]
    pub const fn workspace_read() -> Self {
        Self {
            sandbox: SandboxRequirement::NotApplicable,
            filesystem: ToolFilesystemPolicy::WorkspaceRead,
            network: ToolNetworkPolicy::Denied,
            launches_processes: false,
        }
    }

    #[must_use]
    pub const fn workspace_write() -> Self {
        Self {
            sandbox: SandboxRequirement::NotApplicable,
            filesystem: ToolFilesystemPolicy::WorkspaceWrite,
            network: ToolNetworkPolicy::Denied,
            launches_processes: false,
        }
    }

    #[must_use]
    pub const fn process() -> Self {
        Self {
            sandbox: SandboxRequirement::Required,
            filesystem: ToolFilesystemPolicy::WorkspaceWrite,
            network: ToolNetworkPolicy::Denied,
            launches_processes: true,
        }
    }

    #[must_use]
    pub const fn read_only_process() -> Self {
        Self {
            sandbox: SandboxRequirement::Required,
            filesystem: ToolFilesystemPolicy::WorkspaceRead,
            network: ToolNetworkPolicy::Denied,
            launches_processes: true,
        }
    }
}

/// Stable model-visible and authorization-visible definition of one tool.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: JsonValue,
    pub exposure: ToolExposure,
    pub search_hint: String,
    pub invocation_class: ToolInvocationClass,
}

impl ToolSpec {
    #[must_use]
    pub fn immediate(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: JsonValue,
        invocation_class: ToolInvocationClass,
    ) -> Self {
        let description = description.into();
        Self {
            name: name.into(),
            search_hint: description.clone(),
            description,
            input_schema,
            exposure: ToolExposure::Immediate,
            invocation_class,
        }
    }

    #[must_use]
    pub fn strict(
        name: impl Into<String>,
        description: impl Into<String>,
        mut input_schema: JsonValue,
        invocation_class: ToolInvocationClass,
    ) -> Self {
        if let Some(schema) = input_schema.as_object_mut()
            && schema.get("type").and_then(JsonValue::as_str) == Some("object")
        {
            schema
                .entry("additionalProperties")
                .or_insert(JsonValue::Bool(false));
        }
        Self::immediate(name, description, input_schema, invocation_class)
    }
}

/// Trusted output of tool-specific input preparation.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolPreparation {
    input: JsonValue,
    execution_mode: ToolExecutionMode,
    execution_policy: ToolExecutionPolicy,
}

impl ToolPreparation {
    #[must_use]
    pub const fn new(
        input: JsonValue,
        execution_mode: ToolExecutionMode,
        execution_policy: ToolExecutionPolicy,
    ) -> Self {
        Self {
            input,
            execution_mode,
            execution_policy,
        }
    }

    #[must_use]
    pub const fn with_execution(
        mut self,
        execution_mode: ToolExecutionMode,
        execution_policy: ToolExecutionPolicy,
    ) -> Self {
        self.execution_mode = execution_mode;
        self.execution_policy = execution_policy;
        self
    }
}

/// Failure while converting untrusted model input into a prepared call.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ToolPrepareError {
    #[error("tool `{0}` is unavailable")]
    Unavailable(String),
    #[error("tool input is invalid: {0}")]
    InvalidInput(String),
}

/// Trusted execution environment cannot enforce a prepared call's policy.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ToolEnvironmentError {
    #[error("tool `{0}` requires an OS-enforced process sandbox")]
    SandboxUnavailable(String),
    #[error("tool `{0}` requires approved full network access, which this sandbox cannot enforce")]
    NetworkPolicyUnavailable(String),
}

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

/// Stable specification and trusted input-preparation boundary.
pub trait ToolDefinition: Send + Sync {
    fn spec(&self) -> ToolSpec;

    fn prepare(&self, input: JsonValue) -> Result<ToolPreparation, ToolPrepareError> {
        let spec = self.spec();
        prepare_from_spec(&spec, input)
    }
}

pub(crate) fn prepare_from_spec(
    spec: &ToolSpec,
    input: JsonValue,
) -> Result<ToolPreparation, ToolPrepareError> {
    validate_input_value(&spec.input_schema, &input, "input")?;
    let class = spec.invocation_class;
    let execution_mode = match class {
        ToolInvocationClass::Read
        | ToolInvocationClass::Control
        | ToolInvocationClass::Extension => ToolExecutionMode::Parallel,
        ToolInvocationClass::FilesystemMutation
        | ToolInvocationClass::Terminal
        | ToolInvocationClass::Browser
        | ToolInvocationClass::HostControl
        | ToolInvocationClass::ArbitraryMcp
        | ToolInvocationClass::MemoryCandidate => ToolExecutionMode::Exclusive,
    };
    let execution_policy = match class {
        ToolInvocationClass::Read => ToolExecutionPolicy::workspace_read(),
        ToolInvocationClass::FilesystemMutation => ToolExecutionPolicy::workspace_write(),
        ToolInvocationClass::Terminal => ToolExecutionPolicy::process(),
        ToolInvocationClass::Browser
        | ToolInvocationClass::HostControl
        | ToolInvocationClass::ArbitraryMcp
        | ToolInvocationClass::MemoryCandidate
        | ToolInvocationClass::Control
        | ToolInvocationClass::Extension => ToolExecutionPolicy::in_process(),
    };
    Ok(ToolPreparation::new(
        input,
        execution_mode,
        execution_policy,
    ))
}

fn validate_input_value(
    schema: &JsonValue,
    value: &JsonValue,
    path: &str,
) -> Result<(), ToolPrepareError> {
    if let Some(allowed) = schema.get("enum").and_then(JsonValue::as_array)
        && !allowed.contains(value)
    {
        return Err(ToolPrepareError::InvalidInput(format!(
            "`{path}` is not an allowed value"
        )));
    }
    match schema.get("type").and_then(JsonValue::as_str) {
        Some("object") => validate_object_input(schema, value, path),
        Some("array") => validate_array_input(schema, value, path),
        Some("string") => validate_string_input(schema, value, path),
        Some("integer") if value.as_i64().is_some() || value.as_u64().is_some() => {
            validate_numeric_bounds(schema, value, path)
        }
        Some("number") if value.is_number() => validate_numeric_bounds(schema, value, path),
        Some("boolean") if value.is_boolean() => Ok(()),
        Some("null") if value.is_null() => Ok(()),
        Some(expected) => Err(ToolPrepareError::InvalidInput(format!(
            "`{path}` must be {expected}"
        ))),
        None => Ok(()),
    }
}

fn validate_object_input(
    schema: &JsonValue,
    value: &JsonValue,
    path: &str,
) -> Result<(), ToolPrepareError> {
    let object = value
        .as_object()
        .ok_or_else(|| ToolPrepareError::InvalidInput(format!("`{path}` must be an object")))?;
    let properties = schema.get("properties").and_then(JsonValue::as_object);
    if let Some(required) = schema.get("required").and_then(JsonValue::as_array) {
        for field in required.iter().filter_map(JsonValue::as_str) {
            if !object.contains_key(field) {
                return Err(ToolPrepareError::InvalidInput(format!(
                    "missing required field `{field}`"
                )));
            }
        }
    }
    for (field, field_value) in object {
        if let Some(field_schema) = properties.and_then(|fields| fields.get(field)) {
            validate_input_value(field_schema, field_value, field)?;
            continue;
        }
        match schema.get("additionalProperties") {
            Some(JsonValue::Bool(false)) => {
                return Err(ToolPrepareError::InvalidInput(format!(
                    "unknown field `{field}`"
                )));
            }
            Some(additional_schema) if additional_schema.is_object() => {
                validate_input_value(additional_schema, field_value, field)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_array_input(
    schema: &JsonValue,
    value: &JsonValue,
    path: &str,
) -> Result<(), ToolPrepareError> {
    let values = value
        .as_array()
        .ok_or_else(|| ToolPrepareError::InvalidInput(format!("`{path}` must be an array")))?;
    if let Some(minimum) = schema.get("minItems").and_then(JsonValue::as_u64)
        && values.len() < minimum as usize
    {
        return Err(ToolPrepareError::InvalidInput(format!(
            "`{path}` has fewer than {minimum} items"
        )));
    }
    if let Some(items) = schema.get("items") {
        for item in values {
            validate_input_value(items, item, path)?;
        }
    }
    Ok(())
}

fn validate_string_input(
    schema: &JsonValue,
    value: &JsonValue,
    path: &str,
) -> Result<(), ToolPrepareError> {
    let text = value
        .as_str()
        .ok_or_else(|| ToolPrepareError::InvalidInput(format!("`{path}` must be a string")))?;
    let length = text.chars().count();
    if let Some(minimum) = schema.get("minLength").and_then(JsonValue::as_u64)
        && length < minimum as usize
    {
        return Err(ToolPrepareError::InvalidInput(format!(
            "`{path}` is shorter than {minimum} characters"
        )));
    }
    if let Some(maximum) = schema.get("maxLength").and_then(JsonValue::as_u64)
        && length > maximum as usize
    {
        return Err(ToolPrepareError::InvalidInput(format!(
            "`{path}` is longer than {maximum} characters"
        )));
    }
    Ok(())
}

fn validate_numeric_bounds(
    schema: &JsonValue,
    value: &JsonValue,
    path: &str,
) -> Result<(), ToolPrepareError> {
    let number = value
        .as_f64()
        .ok_or_else(|| ToolPrepareError::InvalidInput(format!("`{path}` must be numeric")))?;
    if let Some(minimum) = schema.get("minimum").and_then(JsonValue::as_f64)
        && number < minimum
    {
        return Err(ToolPrepareError::InvalidInput(format!(
            "`{path}` is below its minimum"
        )));
    }
    if let Some(maximum) = schema.get("maximum").and_then(JsonValue::as_f64)
        && number > maximum
    {
        return Err(ToolPrepareError::InvalidInput(format!(
            "`{path}` exceeds its maximum"
        )));
    }
    Ok(())
}

/// Handler that can execute only an already prepared immutable call.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn handle(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError>;

    async fn handle_streaming(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
        _progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        self.handle(ctx, call).await
    }
}

/// Complete registered implementation. The registry binds both halves once.
pub trait RegisteredTool: ToolDefinition + ToolExecutor {}

impl<T> RegisteredTool for T where T: ToolDefinition + ToolExecutor {}

impl<T> ToolDefinition for Arc<T>
where
    T: RegisteredTool + ?Sized,
{
    fn spec(&self) -> ToolSpec {
        self.as_ref().spec()
    }

    fn prepare(&self, input: JsonValue) -> Result<ToolPreparation, ToolPrepareError> {
        self.as_ref().prepare(input)
    }
}

#[async_trait]
impl<T> ToolExecutor for Arc<T>
where
    T: RegisteredTool + ?Sized,
{
    async fn handle(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        self.as_ref().handle(ctx, call).await
    }

    async fn handle_streaming(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
        progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        self.as_ref().handle_streaming(ctx, call, progress).await
    }
}

/// Immutable call used by authorization, scheduling, and execution.
#[derive(Clone)]
pub struct PreparedToolCall {
    implementation: Arc<dyn RegisteredTool>,
    spec: Arc<ToolSpec>,
    input: JsonValue,
    execution_mode: ToolExecutionMode,
    execution_policy: ToolExecutionPolicy,
}

impl PreparedToolCall {
    #[must_use]
    pub fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    #[must_use]
    pub fn input(&self) -> &JsonValue {
        &self.input
    }

    #[must_use]
    pub const fn execution_mode(&self) -> ToolExecutionMode {
        self.execution_mode
    }

    #[must_use]
    pub const fn execution_policy(&self) -> &ToolExecutionPolicy {
        &self.execution_policy
    }

    /// Verify the selected physical executor can enforce this frozen policy.
    pub fn validate_environment(&self, ctx: &ToolContext) -> Result<(), ToolEnvironmentError> {
        if !self.execution_policy.launches_processes {
            return Ok(());
        }
        let isolation = ctx.executor.process_isolation();
        if self.execution_policy.sandbox == SandboxRequirement::Required
            && !isolation.enforces_sandbox()
        {
            return Err(ToolEnvironmentError::SandboxUnavailable(
                self.spec.name.clone(),
            ));
        }
        if self.execution_policy.network == ToolNetworkPolicy::FullAfterApproval {
            return Err(ToolEnvironmentError::NetworkPolicyUnavailable(
                self.spec.name.clone(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn execute_streaming(
        &self,
        ctx: &ToolContext,
        progress: ToolProgressSink,
    ) -> Result<ToolOutput, ToolError> {
        self.implementation
            .handle_streaming(ctx, self, progress)
            .await
    }
}

impl std::fmt::Debug for PreparedToolCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedToolCall")
            .field("name", &self.spec.name)
            .field("execution_mode", &self.execution_mode)
            .field("execution_policy", &self.execution_policy)
            .finish_non_exhaustive()
    }
}

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
    tools: HashMap<String, Arc<dyn RegisteredTool>>,
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
        Ok(PreparedToolCall {
            implementation,
            spec,
            input: preparation.input,
            execution_mode: preparation.execution_mode,
            execution_policy: preparation.execution_policy,
        })
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
#[path = "../tests/unit/tool.rs"]
mod tests;
