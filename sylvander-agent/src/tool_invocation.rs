//! Central authorization boundary for executable tool invocations.
//!
//! The model can suggest a tool name and JSON input, but it cannot authorize
//! either. Every ordinary tool call passes through
//! [`ToolInvocationGateway`](crate::tool_invocation::ToolInvocationGateway)
//! immediately before execution and reports one terminal outcome through the
//! returned
//! [`AuthorizedToolInvocation`](crate::tool_invocation::AuthorizedToolInvocation).
//! Runtime implementations bind the request to a trusted Worker identity and
//! durable content-safe audit sink.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::tool_context::ToolContext;

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

/// Immutable schema and execution-class description for one registered tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationDescriptor {
    /// Exact route advertised to the model.
    pub name: String,
    /// Runtime policy class.
    pub class: ToolInvocationClass,
    /// JSON input schema used to content-address the route.
    pub input_schema: Value,
}

/// Whether one feature in a turn snapshot can execute or only contributes
/// prompt context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityFeatureKind {
    /// An exact executable route.
    Executable(ToolInvocationClass),
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
                kind: CapabilityFeatureKind::Executable(descriptor.class),
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
            .filter(|feature| matches!(feature.kind, CapabilityFeatureKind::Executable(_)))
            .eq(other
                .features
                .iter()
                .filter(|feature| matches!(feature.kind, CapabilityFeatureKind::Executable(_))))
    }

    fn authorizes(&self, name: &str, class: ToolInvocationClass) -> bool {
        self.features.contains(&CapabilityFeature {
            name: name.to_owned(),
            kind: CapabilityFeatureKind::Executable(class),
        })
    }
}

/// Owned authorization request passed from the unique tool execution entry.
#[derive(Debug, Clone)]
pub struct ToolInvocationRequest {
    call_id: String,
    route: String,
    class: Option<ToolInvocationClass>,
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
        context: &ToolContext,
        input: Value,
        snapshot: ToolInvocationSnapshot,
    ) -> Self {
        Self {
            call_id: call_id.to_owned(),
            route: route.to_owned(),
            class,
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
pub(crate) struct RegistryBoundToolGateway {
    descriptors: BTreeMap<String, ToolInvocationDescriptor>,
    snapshot: ToolInvocationSnapshot,
}

impl RegistryBoundToolGateway {
    pub(crate) fn new(descriptors: Vec<ToolInvocationDescriptor>) -> Arc<Self> {
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
        let descriptor = self
            .descriptors
            .get(&request.route)
            .ok_or(ToolInvocationError::Unavailable)?;
        if descriptor.class != class
            || !request.snapshot.authorizes(&request.route, class)
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

fn snapshot_revision(
    base_revision: &str,
    tool_surface_revision: &str,
    features: &BTreeSet<CapabilityFeature>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"sylvander.tool.invocation-snapshot.v1\0");
    hasher.update(base_revision.as_bytes());
    hasher.update([0]);
    hasher.update(tool_surface_revision.as_bytes());
    for feature in features {
        hasher.update([0]);
        hasher.update(match feature.kind {
            CapabilityFeatureKind::Executable(class) => [b'e', invocation_class_code(class)],
            CapabilityFeatureKind::PromptContext => [b'p', 0],
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

#[cfg(test)]
#[path = "../tests/unit/tool_invocation.rs"]
mod tests;
