//! # sylvander-runtime
//!
//! System runtime — the stateful orchestration layer around the stateless
//! Agent execution kernel.
//!
//! The runtime owns Agent and Session lifecycles, durable state, concrete
//! execution environments, protocol channels, and observable shutdown.

/// Runtime-owned Agent definitions, turn application service, and supervision.
pub mod agent;
use agent::administration as agent_admin;
/// Runtime-owned declarative Agent configuration.
pub use agent::definition as agent_definition;
/// Runtime-owned Agent, provider, model, and credential revision governance.
pub mod registry;
/// Versioned Agent definitions and active-revision lookup.
pub use registry::agent as agent_registry;
#[cfg(test)]
use registry::bootstrap as registry_bootstrap;
#[cfg(test)]
use registry::composition as registry_composition_v3;
#[cfg(test)]
use registry::domain as registry_domain;
#[cfg(test)]
use registry::snapshot as agent_registry_snapshot_v3;
#[cfg(test)]
#[path = "../tests/unit/agent_registry_snapshot_v3_contract.rs"]
mod agent_registry_snapshot_v3_tests;
/// Runtime-owned execution facade for one configured Agent revision.
pub use agent::run as agent_run;
/// Runtime-owned lifecycle supervisor for Agents and Sessions.
pub use agent::supervisor as agent_supervisor;
mod capability_runtime;
/// Coding workspace selection, local and remote worktrees, and governed self-change.
mod workspace;
/// Agent-specific worktree provisioning and crash recovery.
pub use workspace::agent_views as agent_workspace;
/// Target-aware local and remote coding-session isolation.
pub use workspace::coding as coding_worktree;
/// Builds configured Agent revisions, prompt layers, providers, and tools.
pub mod composition;
/// Latest-version server configuration and secret-reference contracts.
pub mod config;
/// Governed relationships, delegation, messaging, and arbitration among Agents.
pub mod coordination;
/// Runtime-owned credential resolution, revision state, and content-safe audit.
mod credential;
/// Durable, content-safe Provider and Channel credential operation audit.
pub use credential::audit as credential_audit;
#[cfg(test)]
use credential::registry as credential_registry;
#[cfg(test)]
#[path = "../tests/unit/credential_registry.rs"]
mod credential_registry_tests;
/// Content-safe runtime evidence, feedback, and authorization records.
pub mod evidence;
/// Workspace target selection and execution policy composition.
pub mod execution;
/// Outbound macOS workspace worker client.
pub mod workspace_worker_client;
/// Isolated local Git worktree lease lifecycle for coding sessions.
pub use workspace::local as git_worktree;
mod guardian;
#[cfg(test)]
use guardian::curation as guardian_curation;
#[cfg(test)]
use session::identity_binding as identity_binding_service;
#[cfg(test)]
#[path = "../tests/unit/identity_binding_service.rs"]
mod identity_binding_service_tests;
/// Session-owned MCP lifecycle and process-environment boundary.
pub(crate) mod mcp;
#[cfg(test)]
use mcp::stdio as mcp_stdio;
mod memory_maintenance;
#[cfg(test)]
#[path = "../tests/unit/model_registry.rs"]
mod model_registry_tests;
mod observability;
#[cfg(test)]
#[path = "../tests/unit/observability.rs"]
mod observability_tests;
/// Stable user mapping for authenticated transport principals.
pub use session::principal_binding;
#[cfg(test)]
#[path = "../tests/unit/principal_binding.rs"]
mod principal_binding_tests;
/// Explicit translations between Agent prompt evidence and public DTOs.
pub use agent::prompt as prompt_contract;
/// Provider catalogs, registry state, and request-scoped credential routing.
mod provider;
/// Controlled synchronization of provider model catalogs into the registry.
pub use provider::catalog_sync as provider_catalog_sync;
#[cfg(test)]
#[path = "../tests/unit/provider_registry.rs"]
mod provider_registry_tests;
#[cfg(test)]
#[path = "../tests/unit/registry_bootstrap.rs"]
mod registry_bootstrap_tests;
#[cfg(test)]
#[path = "../tests/unit/registry_composition_v3.rs"]
mod registry_composition_v3_tests;
#[cfg(test)]
#[path = "../tests/unit/registry_domain.rs"]
mod registry_domain_tests;
pub use provider::request_scoped::{
    ExternalSecretLease, ExternalSecretLeaseError, ExternalSecretLeaseFuture,
    MAX_EXTERNAL_SECRET_LEASE_SECONDS, RenewableExternalSecretProvider, SecretLeaseMetadata,
};
/// Durable executor-backed Git worktree leases for remote coding sessions.
pub use workspace::remote as remote_git_worktree;
/// Evidence-backed, human-gated self-change experiments.
pub use workspace::self_change;
/// Runtime Session state and metadata.
pub mod session;
/// Durable Runtime storage contracts and implementations.
pub mod storage;
#[cfg(test)]
#[path = "../tests/unit/storage.rs"]
mod storage_tests;
#[allow(dead_code)]
mod user_profile_store;

mod runtime;
pub use observability::{
    RUNTIME_DURATION_BUCKET_UPPER_BOUNDS_MICROS, RuntimeDurationHistogramSnapshot,
    RuntimeObservabilitySnapshot,
};
#[cfg(test)]
pub(crate) use runtime::configure_test_memory_integrity;
pub use runtime::{
    ApproveAgentWorkspaceRequest, ChannelHealth, ChannelRegistration, ChannelRestartPolicy,
    ChannelStatus, DefinedAgentJoinRequest, ProviderCredentialSources, Runtime, RuntimeError,
    RuntimeHealthIssue, RuntimeOperationalSnapshot, SessionBindingError, SwarmCompositionOutcome,
    SwarmCompositionPlan, SwarmCompositionReceipt, SwarmMemberPlan,
};
#[cfg(test)]
#[path = "../tests/unit/support.rs"]
mod test_support;
