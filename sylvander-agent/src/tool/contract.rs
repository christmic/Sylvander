//! Tool definition, preparation, execution, and registration contracts.
//!
//! Tools are caller-pluggable. Production tools and runtime-owned dynamic
//! sources share this contract; test doubles live below `tests/`.
//!
//! The trait uses `async_trait` for dyn-compatibility + Send safety.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

use crate::execution::tool_context::ToolContext;
use crate::tool::invocation::ToolInvocationClass;

pub(crate) const TOOL_PROGRESS_CHANNEL_CAPACITY: usize = 64;
pub(crate) const TOOL_PROGRESS_OMITTED_MARKER: &str =
    "\n… intermediate tool output omitted because the progress buffer was full …\n";

/// Agent-owned lifecycle boundary for an executable hook.
///
/// Hook timing changes execution behavior and therefore belongs beside the
/// tool state machine. Runtime maps versioned configuration DTOs into this
/// enum while composing an Agent revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentHookPhase {
    BeforeTool,
    AfterTool,
    BeforeTurn,
    AfterTurn,
}

/// Runtime state reported by a dynamic tool source without UI protocol types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSourceStatus {
    Active,
    Configured,
    Degraded,
    Unavailable,
}

/// Execution-domain class of a tool capability source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSourceKind {
    Mcp,
    Hook,
    Extension,
}

/// Redacted execution-domain fact contributed by a dynamic tool source.
///
/// Runtime maps this value to its public inspection API. Keeping credentials
/// and raw command arguments absent makes the contract safe to aggregate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSourceFeature {
    pub kind: ToolSourceKind,
    pub name: String,
    pub status: ToolSourceStatus,
    pub summary: String,
    pub source: Option<String>,
    pub requires_authentication: bool,
    pub capabilities: Vec<String>,
    pub reloadable: bool,
}

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
    /// Trusted instructions contributed only while this tool is visible to
    /// the model. Runtime composes them from the frozen turn registry.
    pub prompt_guidelines: Vec<String>,
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
            prompt_guidelines: Vec::new(),
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

    /// Attach usage rules that must track this tool's actual turn exposure.
    #[must_use]
    pub fn with_prompt_guidelines(
        mut self,
        guidelines: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.prompt_guidelines = guidelines
            .into_iter()
            .map(Into::into)
            .filter(|guideline| !guideline.trim().is_empty())
            .collect();
        self
    }
}

/// Trusted output of tool-specific input preparation.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolPreparation {
    input: JsonValue,
    execution_mode: ToolExecutionMode,
    execution_policy: ToolExecutionPolicy,
    command_risk: crate::execution::risk::CommandRiskAssessment,
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
            command_risk: crate::execution::risk::CommandRiskAssessment::routine(),
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

    /// Attach backend-independent command-risk facts before approval.
    #[must_use]
    pub fn with_command_risk(
        mut self,
        command_risk: crate::execution::risk::CommandRiskAssessment,
    ) -> Self {
        self.command_risk = command_risk;
        self
    }

    pub(crate) fn into_parts(
        self,
    ) -> (
        JsonValue,
        ToolExecutionMode,
        ToolExecutionPolicy,
        crate::execution::risk::CommandRiskAssessment,
    ) {
        (
            self.input,
            self.execution_mode,
            self.execution_policy,
            self.command_risk,
        )
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
    /// Trusted, content-safe classification for a model-visible failure.
    ///
    /// This is deliberately independent from the human-readable content:
    /// Runtime must never infer security facts by parsing error prose.
    failure_kind: Option<ToolFailureKind>,
}

/// Trusted classification for a model-visible tool failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFailureKind {
    /// The tool failed without a stronger, adapter-provided classification.
    Unclassified,
    /// The execution environment explicitly rejected a filesystem boundary.
    FilesystemBoundaryPolicyViolation,
}

impl ToolOutput {
    /// Successful tool output.
    #[must_use]
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            failure_kind: None,
        }
    }

    /// Error tool output — the model sees this as a failure.
    #[must_use]
    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            failure_kind: Some(ToolFailureKind::Unclassified),
        }
    }

    /// Model-visible failure carrying a trusted machine classification.
    #[must_use]
    pub fn classified_err(content: impl Into<String>, kind: ToolFailureKind) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            failure_kind: Some(kind),
        }
    }

    /// Return the trusted failure classification, if this output failed.
    #[must_use]
    pub const fn failure_kind(&self) -> Option<ToolFailureKind> {
        self.failure_kind
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
    command_risk: crate::execution::risk::CommandRiskAssessment,
}

impl PreparedToolCall {
    pub(crate) fn new(
        implementation: Arc<dyn RegisteredTool>,
        spec: Arc<ToolSpec>,
        input: JsonValue,
        execution_mode: ToolExecutionMode,
        execution_policy: ToolExecutionPolicy,
        command_risk: crate::execution::risk::CommandRiskAssessment,
    ) -> Self {
        Self {
            implementation,
            spec,
            input,
            execution_mode,
            execution_policy,
            command_risk,
        }
    }

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

    #[must_use]
    pub const fn command_risk(&self) -> &crate::execution::risk::CommandRiskAssessment {
        &self.command_risk
    }

    /// Verify the selected physical executor can enforce this frozen policy.
    pub fn validate_environment(&self, ctx: &ToolContext) -> Result<(), ToolEnvironmentError> {
        validate_execution_environment(&self.spec.name, &self.execution_policy, ctx)
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

pub(crate) fn validate_execution_environment(
    operation: &str,
    policy: &ToolExecutionPolicy,
    ctx: &ToolContext,
) -> Result<(), ToolEnvironmentError> {
    if !policy.launches_processes {
        return Ok(());
    }
    let isolation = ctx.executor.process_isolation();
    if policy.sandbox == SandboxRequirement::Required && !isolation.enforces_process_sandbox() {
        return Err(ToolEnvironmentError::SandboxUnavailable(
            operation.to_owned(),
        ));
    }
    if policy.network == ToolNetworkPolicy::FullAfterApproval {
        return Err(ToolEnvironmentError::NetworkPolicyUnavailable(
            operation.to_owned(),
        ));
    }
    Ok(())
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
