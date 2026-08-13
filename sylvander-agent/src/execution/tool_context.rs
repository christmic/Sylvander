//! `ToolContext` — trusted runtime context for prepared tool execution.
//!
//! # Two-tier context model
//!
//! Sylvander uses two distinct context types for different scopes:
//!
//! - [`AgentExecutionContext`]
//!   — Runtime-validated actor, logical workspace, capabilities, timeout, and
//!   correlation for one Agent execution. It is deliberately not a wire type.
//!
//! - [`ToolContext`] (this struct) — "everything a single tool
//!   invocation needs": owns an `AgentExecutionContext` for authority +
//!   tool-specific concerns (execution budget, surface capabilities).
//!   Short-lived: created per tool call by the agent loop.
//!
//! Tool implementations should:
//! - Read `ctx.execution.actor.{user_id, agent_id, session_id}` for
//!   namespacing and access control.
//! - Read `ctx.surface.fs_root` for the file root instead of holding
//!   their own `workdir` field.
//! - Respect `ctx.budget.timeout`.
//! - Check `ctx.surface.capabilities` for the operations they need.
//!
//! # Distinction from a product Session
//!
//! Runtime's Session record owns durable lifecycle and API identity.
//! `ToolContext` contains only the trusted execution snapshot and injected
//! ports needed for one prepared call. A tool cannot load or mutate product
//! Session state through this value.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::execution::mutation_journal::WorkspaceMutationJournal;
use crate::execution::workspace::{
    UnavailableExecutor, WorkspaceExecutor, WorkspaceExecutorError, WorkspaceTarget,
};
use crate::turn::execution_context::AgentExecutionContext;

#[cfg(test)]
fn default_workspace_executor() -> Arc<dyn WorkspaceExecutor> {
    Arc::new(crate::test_workspace::TestWorkspaceExecutor)
}

#[cfg(not(test))]
fn default_workspace_executor() -> Arc<dyn WorkspaceExecutor> {
    Arc::new(UnavailableExecutor::new("local"))
}

/// Per-invocation context handed to every registered tool executor.
///
/// Cheap to clone (one `Arc` + a few small values); tools can pass
/// it around freely.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Runtime-issued non-wire identity and authority snapshot.
    pub execution: Arc<AgentExecutionContext>,

    /// Execution budget for this tool call.
    pub budget: ExecutionBudget,

    /// What the tool is allowed to touch in this invocation.
    pub surface: SurfaceView,

    /// Location-neutral executor selected for this invocation.
    pub executor: Arc<dyn WorkspaceExecutor>,

    /// Execution target and workspace binding passed to the executor.
    pub execution_target: WorkspaceTarget,

    /// Optional Runtime-owned durable workspace mutation journal.
    pub workspace_journal: Option<Arc<dyn WorkspaceMutationJournal>>,

    /// Runtime-derived identity used by every memory-store operation. It is
    /// intentionally not replaceable through a public builder or model input.
    memory_context: crate::memory::store::MemoryExecutionContext,
    invocation_call_id: Option<String>,
}

impl ToolContext {
    /// Construct an ordinary caller-owned tool context.
    ///
    /// This context has no relationship-memory authority even when the caller
    /// later adds surface capabilities. Runtime uses [`Self::for_runtime`]
    /// after resolving an authenticated execution. Outside unit tests it also
    /// has no executable workspace adapter until Runtime injects one.
    #[must_use]
    pub fn new(execution: AgentExecutionContext) -> Self {
        let memory_context = crate::memory::store::MemoryExecutionContext::untrusted(&execution);
        Self {
            execution: Arc::new(execution),
            budget: ExecutionBudget::default(),
            surface: SurfaceView::default(),
            executor: default_workspace_executor(),
            execution_target: WorkspaceTarget::local(PathBuf::new(), false),
            workspace_journal: None,
            memory_context,
            invocation_call_id: None,
        }
    }

    /// Construct a context for a Runtime-authenticated execution.
    ///
    /// This is an application boundary API, never a model/tool input field.
    /// Runtime must still bind the selected workspace executor explicitly.
    #[must_use]
    pub fn for_runtime(execution: AgentExecutionContext) -> Self {
        let memory_context =
            crate::memory::store::MemoryExecutionContext::for_runtime_worker(&execution);
        Self {
            execution: Arc::new(execution),
            budget: ExecutionBudget::default(),
            surface: SurfaceView::default(),
            executor: default_workspace_executor(),
            execution_target: WorkspaceTarget::local(PathBuf::new(), false),
            workspace_journal: None,
            memory_context,
            invocation_call_id: None,
        }
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
    ) -> Result<&WorkspaceTarget, WorkspaceExecutorError> {
        if self.execution_target.id.trim().is_empty() {
            return Err(WorkspaceExecutorError::InvalidRequest(
                "execution target id is required".into(),
            ));
        }
        if self.execution_target.workspace_path.as_os_str().is_empty() {
            return Err(WorkspaceExecutorError::InvalidPath(
                "workspace path is required".into(),
            ));
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
    pub fn with_workspace_journal(mut self, journal: Arc<dyn WorkspaceMutationJournal>) -> Self {
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
    pub fn memory_context(&self) -> &crate::memory::store::MemoryExecutionContext {
        &self.memory_context
    }

    // ---- identity shortcuts ----
    // Tools frequently need these; the shortcuts save 50 chars per
    // call site and make the typed read obvious to code review.

    /// Runtime-validated user identity.
    pub fn user_id(&self) -> &str {
        &self.execution.actor.user_id
    }

    /// Runtime-pinned Agent identity.
    pub fn agent_id(&self) -> &str {
        &self.execution.actor.agent_id
    }

    /// Product Session identity used for authorization and correlation only.
    pub fn session_id(&self) -> &str {
        &self.execution.actor.session_id
    }

    /// Runtime-issued turn identity, absent outside admitted Runtime turns.
    #[must_use]
    pub fn turn_id(&self) -> Option<&str> {
        self.execution.turn_id.as_deref()
    }

    /// Bind the Runtime-owned model call identifier for journal and receipt
    /// correlation. The authorization gateway still validates it against the
    /// frozen invocation ledger.
    #[must_use]
    pub fn with_invocation_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.invocation_call_id = Some(call_id.into());
        self
    }

    /// Model call identity bound by the Agent loop, never by tool input.
    #[must_use]
    pub fn invocation_call_id(&self) -> Option<&str> {
        self.invocation_call_id.as_deref()
    }

    /// Runtime-assigned turn correlation identifier.
    pub fn trace_id(&self) -> Option<&str> {
        self.execution.trace_id.as_deref()
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

/// Convenience constructors for explicit restricted execution contexts.
pub mod defaults {
    /// Build an explicit `ToolContext` for trusted system-originated actions
    /// and tests that do not execute workspace tools.
    #[must_use]
    pub fn system_tool_context() -> super::ToolContext {
        super::ToolContext::new(
            crate::turn::execution_context::AgentExecutionContext::restricted_for(
                "__system_user__",
                "__system_agent__",
                "__system_session__",
            ),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../../tests/unit/tool_context.rs"]
mod tests;
