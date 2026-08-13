//! Central authorization boundary for executable tool invocations.
//!
//! The model can suggest a tool name and JSON input, but it cannot authorize
//! either. Every ordinary tool call passes through
//! [`ToolInvocationGateway`]
//! immediately before execution and reports one terminal outcome through the
//! returned
//! [`AuthorizedToolInvocation`].
//! Runtime implementations bind the request to a trusted Worker identity and
//! durable content-safe audit sink.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::execution::tool_context::ToolContext;

/// Security-relevant execution class declared by a tool implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolInvocationClass {
    /// Read-only local or governed data access.
    Read,
    /// Workspace file mutation.
    FilesystemMutation,
    /// Shell, process, or Git execution.
    Terminal,
    /// Browser automation.
    Browser,
    /// Host UI or operating-system control.
    HostControl,
    /// An operation supplied by an external MCP server.
    ArbitraryMcp,
    /// A Worker-authored memory candidate, not canonical-memory mutation.
    MemoryCandidate,
    /// A non-executing interaction marker handled by a typed gate.
    Control,
    /// Another explicitly registered extension.
    Extension,
}

/// Permission-independent recovery contract declared by a tool.
///
/// This value describes whether an invocation whose effect is uncertain may
/// execute again. It never grants execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolRecoveryPolicy {
    /// Never execute again after the effect might have started.
    NeverReplay,
    /// Re-execute only with the exact same stable Runtime invocation identity.
    RetryWithSameInvocation,
    /// Reconcile a durable receipt or journal before deciding whether to retry.
    ReconcileBeforeRetry,
}

/// Immutable schema, authority, and recovery description for one registered tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationDescriptor {
    /// Exact route advertised to the model.
    pub name: String,
    /// Runtime policy class.
    pub class: ToolInvocationClass,
    /// Recovery behavior, independent from execution authority.
    pub recovery_policy: ToolRecoveryPolicy,
    /// JSON input schema used to content-address the route.
    pub input_schema: Value,
}

/// Whether one feature in a turn snapshot can execute or only contributes
/// prompt context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityFeatureKind {
    /// An exact executable route.
    Executable(ToolInvocationClass, ToolRecoveryPolicy),
    /// A Skill loaded as prompt context; it grants no execution authority.
    PromptContext,
}

/// Content-safe feature entry bound into one immutable turn revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CapabilityFeature {
    /// Exact feature name.
    pub name: String,
    /// Whether the feature is executable or prompt-only.
    pub kind: CapabilityFeatureKind,
}

/// Immutable capability truth used for every invocation in one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationSnapshot {
    revision: String,
    features: BTreeSet<CapabilityFeature>,
}

impl ToolInvocationSnapshot {
    /// Build a content-addressed executable snapshot from exact tool routes.
    #[must_use]
    pub fn from_descriptors(descriptors: &[ToolInvocationDescriptor]) -> Self {
        let features = descriptors
            .iter()
            .map(|descriptor| CapabilityFeature {
                name: descriptor.name.clone(),
                kind: CapabilityFeatureKind::Executable(
                    descriptor.class,
                    descriptor.recovery_policy,
                ),
            })
            .collect::<BTreeSet<_>>();
        let revision = snapshot_revision("base", "", &features);
        Self { revision, features }
    }

    /// Freeze the executable catalog, hook/schema revision, and discovered
    /// prompt-only Skills into the exact revision used by a turn.
    #[must_use]
    pub fn for_turn(
        &self,
        tool_surface_revision: &str,
        prompt_context_features: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut features = self.features.clone();
        features.extend(
            prompt_context_features
                .into_iter()
                .map(|name| CapabilityFeature {
                    name,
                    kind: CapabilityFeatureKind::PromptContext,
                }),
        );
        let revision = snapshot_revision(&self.revision, tool_surface_revision, &features);
        Self { revision, features }
    }

    /// Content-addressed revision written to approval and invocation audits.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Content-safe feature inventory. Prompt-context features are explicitly
    /// distinct from executable routes.
    #[must_use]
    pub fn features(&self) -> &BTreeSet<CapabilityFeature> {
        &self.features
    }

    /// Return whether another snapshot exposes exactly the same executable
    /// routes and execution classes. Prompt-context features are deliberately
    /// ignored because Runtime freezes the executable catalog before Skills
    /// are discovered for a turn.
    #[must_use]
    pub fn has_same_executable_surface(&self, other: &Self) -> bool {
        self.features
            .iter()
            .filter(|feature| matches!(feature.kind, CapabilityFeatureKind::Executable(..)))
            .eq(other
                .features
                .iter()
                .filter(|feature| matches!(feature.kind, CapabilityFeatureKind::Executable(..))))
    }

    /// Test whether one exact executable route belongs to this frozen surface.
    /// Runtime uses this when atomically installing Session-owned extensions.
    #[must_use]
    pub fn authorizes(
        &self,
        name: &str,
        class: ToolInvocationClass,
        recovery_policy: ToolRecoveryPolicy,
    ) -> bool {
        self.features.contains(&CapabilityFeature {
            name: name.to_owned(),
            kind: CapabilityFeatureKind::Executable(class, recovery_policy),
        })
    }
}

/// Owned authorization request passed from the unique tool execution entry.
#[derive(Debug, Clone)]
pub struct ToolInvocationRequest {
    call_id: String,
    route: String,
    class: Option<ToolInvocationClass>,
    recovery_policy: Option<ToolRecoveryPolicy>,
    context: ToolContext,
    input: Value,
    snapshot: ToolInvocationSnapshot,
}

impl ToolInvocationRequest {
    /// Build an authorization request at the unique tool execution boundary.
    ///
    /// Constructing a request grants no authority: Runtime derives the actor
    /// and owner from [`ToolContext`] and revalidates the exact executable
    /// surface before returning a grant.
    #[must_use]
    pub fn new(
        call_id: &str,
        route: &str,
        class: Option<ToolInvocationClass>,
        recovery_policy: Option<ToolRecoveryPolicy>,
        context: &ToolContext,
        input: Value,
        snapshot: ToolInvocationSnapshot,
    ) -> Self {
        Self {
            call_id: call_id.to_owned(),
            route: route.to_owned(),
            class,
            recovery_policy,
            context: context.clone(),
            input,
            snapshot,
        }
    }

    /// Model-provider call identifier, used only for correlation.
    #[must_use]
    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    /// Exact tool route.
    #[must_use]
    pub fn route(&self) -> &str {
        &self.route
    }

    /// Tool-declared execution class. `None` means no registered tool exists.
    #[must_use]
    pub const fn class(&self) -> Option<ToolInvocationClass> {
        self.class
    }

    /// Tool-declared recovery contract. `None` means no registered tool exists.
    #[must_use]
    pub const fn recovery_policy(&self) -> Option<ToolRecoveryPolicy> {
        self.recovery_policy
    }

    /// Runtime-created invocation context.
    #[must_use]
    pub fn context(&self) -> &ToolContext {
        &self.context
    }

    /// Untrusted model input subject to owner-selector rejection.
    #[must_use]
    pub fn input(&self) -> &Value {
        &self.input
    }

    /// Content-safe digest of the exact prepared input.
    #[must_use]
    pub fn input_digest(&self) -> String {
        prepared_input_digest(&self.input)
    }

    /// Exact immutable snapshot for this turn.
    #[must_use]
    pub fn snapshot(&self) -> &ToolInvocationSnapshot {
        &self.snapshot
    }
}

/// Content-safe terminal state written after an authorized execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolInvocationOutcome {
    /// Tool returned a successful model-visible result.
    Succeeded,
    /// Tool returned or raised a failure.
    Failed,
    /// Execution exceeded its budget and was cancelled.
    TimedOut,
}

/// Authorization/audit failure exposed without request or result content.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum ToolInvocationError {
    /// No exact registered route exists in the immutable snapshot.
    #[error("tool capability is unavailable")]
    Unavailable,
    /// Actor, owner, class, or model input failed policy.
    #[error("tool capability access denied")]
    AccessDenied,
    /// Pre-execution audit could not be made durable.
    #[error("tool capability audit is unavailable")]
    AuditUnavailable,
    /// The tool ran, but its terminal audit could not be made durable.
    #[error("tool execution outcome is uncertain")]
    ExecutionOutcomeUncertain,
}

/// One authorized invocation. Dropping an unfinished Runtime implementation
/// must record a failed/cancelled terminal audit without replaying the tool.
#[async_trait]
pub trait AuthorizedToolInvocation: Send {
    /// Persist the one terminal outcome. Implementations consume the grant so
    /// a caller cannot report two outcomes.
    async fn finish(
        self: Box<Self>,
        outcome: ToolInvocationOutcome,
    ) -> Result<(), ToolInvocationError>;
}

/// Object-safe Runtime boundary for all executable tools.
#[async_trait]
pub trait ToolInvocationGateway: Send + Sync {
    /// Immutable executable feature snapshot owned by this Agent revision.
    fn snapshot(&self) -> ToolInvocationSnapshot;

    /// Re-authorize one request and durably record its pre-execution audit.
    async fn authorize(
        &self,
        request: ToolInvocationRequest,
    ) -> Result<Box<dyn AuthorizedToolInvocation>, ToolInvocationError>;
}

/// Exact-route gateway used by standalone `AgentLoop` embeddings.
///
/// Runtime production composition always replaces this with its actor-aware,
/// durably audited implementation. This fallback still fails closed for
/// unknown routes, class changes, owner selectors, and snapshot drift.
pub struct RegistryBoundToolGateway {
    descriptors: BTreeMap<String, ToolInvocationDescriptor>,
    snapshot: ToolInvocationSnapshot,
}

impl RegistryBoundToolGateway {
    /// Construct a fail-closed in-process gateway for a fixed tool surface.
    #[must_use]
    pub fn new(descriptors: Vec<ToolInvocationDescriptor>) -> Arc<Self> {
        let snapshot = ToolInvocationSnapshot::from_descriptors(&descriptors);
        let descriptors = descriptors
            .into_iter()
            .map(|descriptor| (descriptor.name.clone(), descriptor))
            .collect();
        Arc::new(Self {
            descriptors,
            snapshot,
        })
    }
}

struct RegistryBoundGrant;

#[async_trait]
impl AuthorizedToolInvocation for RegistryBoundGrant {
    async fn finish(
        self: Box<Self>,
        _outcome: ToolInvocationOutcome,
    ) -> Result<(), ToolInvocationError> {
        Ok(())
    }
}

#[async_trait]
impl ToolInvocationGateway for RegistryBoundToolGateway {
    fn snapshot(&self) -> ToolInvocationSnapshot {
        self.snapshot.clone()
    }

    async fn authorize(
        &self,
        request: ToolInvocationRequest,
    ) -> Result<Box<dyn AuthorizedToolInvocation>, ToolInvocationError> {
        let class = request.class.ok_or(ToolInvocationError::Unavailable)?;
        let recovery_policy = request
            .recovery_policy
            .ok_or(ToolInvocationError::Unavailable)?;
        let descriptor = self
            .descriptors
            .get(&request.route)
            .ok_or(ToolInvocationError::Unavailable)?;
        if descriptor.class != class
            || descriptor.recovery_policy != recovery_policy
            || !request
                .snapshot
                .authorizes(&request.route, class, recovery_policy)
            || contains_owner_selector(&request.input)
        {
            return Err(ToolInvocationError::AccessDenied);
        }
        Ok(Box::new(RegistryBoundGrant))
    }
}

fn contains_owner_selector(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "owner" | "owner_id" | "user_id" | "agent_id" | "session_id" | "workspace_id"
            ) || contains_owner_selector(value)
        }),
        Value::Array(values) => values.iter().any(contains_owner_selector),
        _ => false,
    }
}

/// Domain-separated digest shared by Agent events and Runtime authorization.
#[must_use]
pub fn prepared_input_digest(input: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander.tool.prepared-input.v1\0");
    hasher.update(serde_json::to_vec(input).unwrap_or_default());
    format!("sha256:{:x}", hasher.finalize())
}

fn snapshot_revision(
    base_revision: &str,
    tool_surface_revision: &str,
    features: &BTreeSet<CapabilityFeature>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander.tool.invocation-snapshot.v2\0");
    hasher.update(base_revision.as_bytes());
    hasher.update([0]);
    hasher.update(tool_surface_revision.as_bytes());
    for feature in features {
        hasher.update([0]);
        hasher.update(match feature.kind {
            CapabilityFeatureKind::Executable(class, policy) => [
                b'e',
                invocation_class_code(class),
                recovery_policy_code(policy),
            ],
            CapabilityFeatureKind::PromptContext => [b'p', 0, 0],
        });
        hasher.update(feature.name.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

const fn invocation_class_code(class: ToolInvocationClass) -> u8 {
    match class {
        ToolInvocationClass::Read => 1,
        ToolInvocationClass::FilesystemMutation => 2,
        ToolInvocationClass::Terminal => 3,
        ToolInvocationClass::Browser => 4,
        ToolInvocationClass::HostControl => 5,
        ToolInvocationClass::ArbitraryMcp => 6,
        ToolInvocationClass::MemoryCandidate => 7,
        ToolInvocationClass::Control => 8,
        ToolInvocationClass::Extension => 9,
    }
}

const fn recovery_policy_code(policy: ToolRecoveryPolicy) -> u8 {
    match policy {
        ToolRecoveryPolicy::NeverReplay => 1,
        ToolRecoveryPolicy::RetryWithSameInvocation => 2,
        ToolRecoveryPolicy::ReconcileBeforeRetry => 3,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/tool_invocation.rs"]
mod tests;
