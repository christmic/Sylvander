//! `ToolContext` — per-invocation input to a `Tool::execute` call.
//!
//! # Two-tier context model
//!
//! Sylvander uses two distinct context types for different scopes:
//!
//! - [`sylvander_protocol::SessionContext`] — "who, where, when, why":
//!   identity, origin, request metadata, free-form attributes. Lives
//!   the entire session. Cross-crate. Adds fields without breaking
//!   call sites.
//!
//! - [`ToolContext`](crate::tool_context::ToolContext) (this struct) — "everything a single tool
//!   invocation needs": owns a `SessionContext` for identity +
//!   tool-specific concerns (execution budget, surface capabilities).
//!   Short-lived: created per tool call by the agent loop.
//!
//! Tool implementations should:
//! - Read `ctx.session.identity.{user_id, agent_id, session_id}` for
//!   namespacing and access control.
//! - Read `ctx.surface.fs_root` for the file root instead of holding
//!   their own `workdir` field.
//! - Respect `ctx.budget.timeout`.
//! - Check `ctx.surface.capabilities` for the operations they need.
//!
//! # Distinction from `SessionContext`
//!
//! `SessionContext` is "who is asking"; `ToolContext` is "everything
//! the tool needs to run". Adding tool-specific fields (cancellation
//! tokens, retry budgets, sandboxing) goes here, not in
//! `SessionContext`. Adding identity / origin fields goes in
//! `SessionContext`. The split is stable: new fields never have to
//! cross the line.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sylvander_protocol::SessionContext;

use crate::workspace_executor::{
    LocalExecutor, UnavailableExecutor, WorkspaceExecutor, WorkspaceTarget,
};

/// Per-invocation context handed to every `Tool::execute` call.
///
/// Cheap to clone (one `Arc` + a few small values); tools can pass
/// it around freely.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Session-scoped identity / origin / request metadata.
    /// Wrapped in `Arc` so a tool can hold a reference past the
    /// invocation lifetime (e.g. for async background work).
    pub session: Arc<SessionContext>,

    /// Execution budget for this tool call.
    pub budget: ExecutionBudget,

    /// What the tool is allowed to touch in this invocation.
    pub surface: SurfaceView,

    /// Location-neutral executor selected for this invocation.
    pub executor: Arc<dyn WorkspaceExecutor>,

    /// Execution target and workspace binding passed to the executor.
    pub execution_target: WorkspaceTarget,

    /// Optional durable workspace mutation journal owned by the Agent runtime.
    pub workspace_journal: Option<Arc<crate::workspace_journal::WorkspaceJournal>>,

    /// Runtime-derived identity used by every memory-store operation. It is
    /// intentionally not replaceable through a public builder or model input.
    memory_context: crate::tools::memory::MemoryExecutionContext,

    /// `AgentRun` needs an `AgentLoop` value before any session exists. That
    /// construction-only template is deliberately unusable: `run_stream`
    /// rejects it before hooks, model requests, or tools can execute. Every
    /// real turn replaces it with a Runtime-derived context.
    inert_agent_run_template: bool,
}

impl ToolContext {
    /// Construct an ordinary caller-owned tool context.
    ///
    /// This context has no relationship-memory authority even when the caller
    /// later adds surface capabilities. Agent application code uses the
    /// crate-private constructor below after resolving a real session.
    #[must_use]
    pub fn new(session: SessionContext) -> Self {
        let memory_context = crate::tools::memory::MemoryExecutionContext::untrusted(&session);
        Self {
            session: Arc::new(session),
            budget: ExecutionBudget::default(),
            surface: SurfaceView::default(),
            executor: Arc::new(LocalExecutor),
            execution_target: WorkspaceTarget::local(PathBuf::new(), false),
            workspace_journal: None,
            memory_context,
            inert_agent_run_template: false,
        }
    }

    #[must_use]
    pub(crate) fn application(session: SessionContext) -> Self {
        let memory_context =
            crate::tools::memory::MemoryExecutionContext::application_worker(&session);
        Self {
            session: Arc::new(session),
            budget: ExecutionBudget::default(),
            surface: SurfaceView::default(),
            executor: Arc::new(LocalExecutor),
            execution_target: WorkspaceTarget::local(PathBuf::new(), false),
            workspace_journal: None,
            memory_context,
            inert_agent_run_template: false,
        }
    }

    /// Create the construction-only context stored by `AgentRun` before a
    /// session turn resolves its authenticated identity and workspace.
    ///
    /// This stays crate-private so embeddings cannot opt into a placeholder
    /// identity. `AgentLoop::run_stream` refuses to run with this sentinel.
    #[must_use]
    pub(crate) fn inert_agent_run_template() -> Self {
        let mut context = Self::new(SessionContext::system());
        context.executor = Arc::new(UnavailableExecutor::new("__inert_agent_run_template__"));
        context.execution_target = WorkspaceTarget {
            id: "__inert_agent_run_template__".into(),
            workspace_path: PathBuf::new(),
            read_only: true,
        };
        context.inert_agent_run_template = true;
        context
    }

    #[must_use]
    pub(crate) fn is_inert_agent_run_template(&self) -> bool {
        self.inert_agent_run_template
    }

    /// Builder-style: attach a file-system root to the surface.
    #[must_use]
    pub fn with_fs_root(mut self, root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        self.surface.fs_root = Some(root.clone());
        self.execution_target.workspace_path = root;
        self
    }

    /// Bind this invocation to a named execution target.
    #[must_use]
    pub fn with_execution_target(
        mut self,
        target_id: impl Into<String>,
        workspace_path: impl Into<PathBuf>,
        read_only: bool,
    ) -> Self {
        let target_id = target_id.into();
        // A named target is meaningful only together with an executor chosen
        // by the owning runtime. Keep this convenience fail-closed instead of
        // embedding target-id routing policy in a per-tool value object.
        self.executor = Arc::new(UnavailableExecutor::new(target_id.clone()));
        self.execution_target = WorkspaceTarget {
            id: target_id,
            workspace_path: workspace_path.into(),
            read_only,
        };
        self.surface.fs_root = Some(self.execution_target.workspace_path.clone());
        self
    }

    /// Inject an executor, primarily for transport adapters and contract tests.
    #[must_use]
    pub fn with_executor(
        mut self,
        executor: Arc<dyn WorkspaceExecutor>,
        target: WorkspaceTarget,
    ) -> Self {
        self.surface.fs_root = Some(target.workspace_path.clone());
        self.executor = executor;
        self.execution_target = target;
        self
    }

    /// Return the explicit Runtime-selected workspace target.
    ///
    /// An empty target never means the process working directory and never
    /// falls back to a value captured by a tool. Filesystem and command tools
    /// call this before touching an executor so missing workspace composition
    /// fails closed.
    pub(crate) fn require_execution_target(
        &self,
    ) -> Result<&WorkspaceTarget, crate::workspace_executor::WorkspaceExecutorError> {
        if self.execution_target.id.trim().is_empty() {
            return Err(
                crate::workspace_executor::WorkspaceExecutorError::InvalidRequest(
                    "execution target id is required".into(),
                ),
            );
        }
        if self.execution_target.workspace_path.as_os_str().is_empty() {
            return Err(
                crate::workspace_executor::WorkspaceExecutorError::InvalidPath(
                    "workspace path is required".into(),
                ),
            );
        }
        Ok(&self.execution_target)
    }

    /// Builder-style: attach an execution budget.
    #[must_use]
    pub fn with_budget(mut self, budget: ExecutionBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Builder-style: grant a capability.
    #[must_use]
    pub fn with_capability(mut self, cap: Cap) -> Self {
        self.surface.capabilities.insert(cap);
        self
    }

    #[must_use]
    pub fn with_workspace_journal(
        mut self,
        journal: Arc<crate::workspace_journal::WorkspaceJournal>,
    ) -> Self {
        self.workspace_journal = Some(journal);
        self
    }

    /// Cheaply wrap in `Arc` for shared ownership across tool copies.
    #[must_use]
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    /// Runtime-derived memory identity for this invocation.
    #[must_use]
    pub fn memory_context(&self) -> &crate::tools::memory::MemoryExecutionContext {
        &self.memory_context
    }

    // ---- identity shortcuts ----
    // Tools frequently need these; the shortcuts save 50 chars per
    // call site and make the typed read obvious to code review.

    /// Convenience: `ctx.session.identity.user_id`.
    pub fn user_id(&self) -> &sylvander_protocol::types::UserId {
        &self.session.identity.user_id
    }

    /// Convenience: `ctx.session.identity.agent_id`.
    pub fn agent_id(&self) -> &sylvander_protocol::types::AgentId {
        &self.session.identity.agent_id
    }

    /// Convenience: `ctx.session.identity.session_id`.
    pub fn session_id(&self) -> &sylvander_protocol::types::SessionId {
        &self.session.identity.session_id
    }
}

// ---------------------------------------------------------------------------
// ExecutionBudget
// ---------------------------------------------------------------------------

/// Per-call execution limits.
///
/// Tools should respect `timeout` by wrapping their long work in
/// `tokio::time::timeout`. `max_retries` is a hint for tools that
/// implement their own retry (network calls, etc.).
#[derive(Debug, Clone)]
pub struct ExecutionBudget {
    /// Hard deadline for this tool call. `None` = no timeout.
    pub timeout: Option<Duration>,
    /// Maximum retries on transient failure. 0 = no retry.
    pub max_retries: u32,
}

impl Default for ExecutionBudget {
    fn default() -> Self {
        // Matches the upstream loop's TOOL_TIMEOUT default.
        Self {
            timeout: Some(Duration::from_mins(2)),
            max_retries: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// SurfaceView + Cap
// ---------------------------------------------------------------------------

/// What the tool is allowed to do / see in this invocation.
///
/// Tools should check `capabilities` before performing the operation
/// (e.g. `WriteTool` should refuse if `Cap::Write` is absent).
#[derive(Debug, Clone, Default)]
pub struct SurfaceView {
    /// File-system root for this invocation. Tools that touch the
    /// filesystem should resolve relative paths against this.
    pub fs_root: Option<PathBuf>,

    /// Granted capabilities. Empty = sandboxed (no operations allowed).
    pub capabilities: BTreeSet<Cap>,

    /// Network policy (which hosts the tool may reach).
    pub network: NetworkPolicy,
}

/// Operations a tool may perform on behalf of the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cap {
    /// Read files within `fs_root`.
    Read,
    /// Write / create / delete files within `fs_root`.
    Write,
    /// Open outbound network connections.
    Network,
    /// Spawn subprocesses (bash, etc.).
    Spawn,
    /// Run git operations inside `fs_root`.
    Git,
    /// Read from the agent's long-term memory.
    MemoryRead,
    /// Write to the agent's long-term memory.
    MemoryWrite,
    /// Read from the session store / message history.
    SessionRead,
    /// Write to the session store (append messages, archive, etc.).
    SessionWrite,
}

/// Network reachability policy.
#[derive(Debug, Clone, Default)]
pub enum NetworkPolicy {
    /// No network access.
    #[default]
    None,
    /// All hosts reachable.
    All,
    /// Only listed host patterns (exact match for MVP).
    Allow(Vec<String>),
}

impl ToolContext {
    /// `true` if the given capability is granted.
    #[must_use]
    pub fn has_cap(&self, cap: Cap) -> bool {
        self.surface.capabilities.contains(&cap)
    }

    /// `true` if the given host is allowed by the network policy.
    #[must_use]
    pub fn host_allowed(&self, host: &str) -> bool {
        match &self.surface.network {
            NetworkPolicy::None => false,
            NetworkPolicy::All => true,
            NetworkPolicy::Allow(list) => list.iter().any(|h| h == host),
        }
    }
}

/// Convenience constructors for `SessionContext` values used when the
/// caller has not supplied one. Kept in their own module so callers
/// don't have to scroll past struct definitions.
pub mod defaults {
    use super::{SessionContext, ToolContext};

    /// Build an explicit `ToolContext` for trusted system-originated actions
    /// and tests that do not execute workspace tools.
    #[must_use]
    pub fn system_tool_context() -> ToolContext {
        ToolContext::new(SessionContext::system())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../tests/unit/tool_context.rs"]
mod tests;
