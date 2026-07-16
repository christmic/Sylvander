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
//! - [`ToolContext`] (this struct) — "everything a single tool
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

use sylvander_llm_anthropic::api::model::ModelInfo;
use sylvander_protocol::SessionContext;

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

    /// Optional durable workspace mutation journal owned by the Agent runtime.
    pub workspace_journal: Option<Arc<crate::workspace_journal::WorkspaceJournal>>,

    /// Runtime-derived identity used by every memory-store operation. It is
    /// intentionally not replaceable through a public builder or model input.
    memory_context: crate::tools::memory::MemoryExecutionContext,
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
            workspace_journal: None,
            memory_context,
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
            workspace_journal: None,
            memory_context,
        }
    }

    /// Builder-style: attach a file-system root to the surface.
    #[must_use]
    pub fn with_fs_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.surface.fs_root = Some(root.into());
        self
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
    use super::{ModelInfo, SessionContext, ToolContext};
    use sylvander_protocol::types::SessionId;

    /// Build a placeholder `ToolContext` for system-originated
    /// actions (cron, internal tasks, model-defaulted loop setup).
    #[must_use]
    pub fn system_tool_context() -> ToolContext {
        ToolContext::new(SessionContext::system())
    }

    /// Build a `ToolContext` for a model-derived agent that has no
    /// real user / session. Used as a fallback by `AgentLoop::build`
    /// when the caller doesn't pass an explicit context.
    #[must_use]
    pub fn model_tool_context(model: &ModelInfo) -> ToolContext {
        let session = SessionContext::new(
            crate::tool_context::defaults::system_user(),
            crate::tool_context::defaults::model_agent(model),
            crate::tool_context::defaults::ephemeral_session(),
        );
        ToolContext::new(session)
    }

    // Re-export the inner helpers so existing callers
    // (`crate::tool_context::defaults::system_user()`) keep working.
    #[must_use]
    pub fn system_user() -> sylvander_protocol::types::UserId {
        sylvander_protocol::types::UserId::system()
    }

    #[must_use]
    pub fn model_agent(model: &ModelInfo) -> sylvander_protocol::types::AgentId {
        sylvander_protocol::types::AgentId::new(format!("model:{}", model.id))
    }

    #[must_use]
    pub fn ephemeral_session() -> SessionId {
        SessionId::new("__ephemeral__")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_protocol::types::{AgentId, SessionId, UserId};

    fn session() -> SessionContext {
        SessionContext::new(
            UserId::new("alice"),
            AgentId::new("code-assistant"),
            SessionId::new("sess-1"),
        )
    }

    #[test]
    fn new_wraps_session_in_arc() {
        let ctx = ToolContext::new(session());
        assert_eq!(ctx.user_id().0, "alice");
        assert_eq!(ctx.agent_id().0, "code-assistant");
        assert_eq!(ctx.session_id().0, "sess-1");
        assert!(ctx.surface.fs_root.is_none());
        assert!(ctx.surface.capabilities.is_empty());
        assert!(matches!(
            ctx.memory_context().relationship_owner(),
            Err(crate::tools::memory::MemoryStoreError::AccessDenied)
        ));
    }

    #[test]
    fn application_context_issues_memory_authority() {
        let ctx = ToolContext::application(session());
        assert_eq!(
            ctx.memory_context().relationship_owner().unwrap(),
            crate::tools::memory::MemoryOwner::Relationship {
                user_id: UserId::new("alice"),
                agent_id: AgentId::new("code-assistant"),
            }
        );
    }

    #[test]
    fn builder_methods_chain() {
        let ctx = ToolContext::new(session())
            .with_fs_root("/home/alice/code")
            .with_capability(Cap::Read)
            .with_capability(Cap::Write);

        assert_eq!(
            ctx.surface.fs_root.as_deref(),
            Some(std::path::Path::new("/home/alice/code"))
        );
        assert!(ctx.has_cap(Cap::Read));
        assert!(ctx.has_cap(Cap::Write));
        assert!(!ctx.has_cap(Cap::Network));
    }

    #[test]
    fn default_budget_has_timeout() {
        let ctx = ToolContext::new(session());
        assert!(ctx.budget.timeout.is_some());
        assert_eq!(ctx.budget.max_retries, 0);
    }

    #[test]
    fn host_allowed_respects_policy() {
        let mut ctx = ToolContext::new(session());
        assert!(!ctx.host_allowed("api.example.com"));

        ctx.surface.network = NetworkPolicy::All;
        assert!(ctx.host_allowed("api.example.com"));

        ctx.surface.network = NetworkPolicy::Allow(vec!["api.openai.com".into()]);
        assert!(ctx.host_allowed("api.openai.com"));
        assert!(!ctx.host_allowed("evil.example.com"));
    }

    #[test]
    fn clones_independently_but_share_session_arc() {
        let ctx = ToolContext::new(session());
        let ctx2 = ctx.clone();
        // Session is Arc-backed so both views see the same identity.
        assert!(Arc::ptr_eq(&ctx.session, &ctx2.session));
    }

    #[test]
    fn system_sentinel_is_distinct() {
        let real = ToolContext::new(session());
        let sys = defaults::system_tool_context();
        assert_ne!(real.user_id(), sys.user_id());
    }
}
