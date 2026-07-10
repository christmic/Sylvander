//! `ToolContext` — passed to every `Tool::execute` call so tools can
//! implement per-user / per-agent / per-session isolation.
//!
//! Why this exists:
//! - Memory and Session stores are scoped resources. Without a
//!   context, a tool can only see global state.
//! - Tool implementations (Read, Write, MemoryRead, MemoryWrite) need
//!   to know *which* user / agent / session is invoking them so they
//!   can enforce permissions, namespace memory, and route file
//!   reads to the correct workspace.
//!
//! Threading model:
//! - `ToolContext` is created per `AgentRun` at startup.
//! - Stored on `AgentRunInner` as `Arc<ToolContext>`.
//! - The agent loop borrows it and passes `&ToolContext` into every
//!   tool call. Tools never clone it; they only read.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use sylvander_protocol::types::{AgentId, SessionId, UserId};

/// Per-invocation context handed to every `Tool::execute`.
///
/// Cheap to clone (it's a few `String`s + a `HashMap`); tools can
/// freely pass it around. The whole struct is `Clone` so tools can
/// also store parts of it (e.g. `agent_id`) in their own state if
/// they need to.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Who triggered this invocation. `UserId::system()` for cron /
    /// internal calls with no real user.
    pub user_id: UserId,

    /// Which agent (within the user's tenant) is running.
    pub agent_id: AgentId,

    /// Which session (conversation) this invocation belongs to.
    pub session_id: SessionId,

    /// Optional workspace root. Tools that touch the filesystem
    /// (Read/Write/Edit) should resolve paths relative to this when
    /// the input uses a relative path.
    pub workspace: Option<PathBuf>,

    /// Free-form metadata (channel name, request id, trace id, …).
    pub metadata: HashMap<String, String>,
}

impl ToolContext {
    /// Convenience constructor for the minimum-required fields.
    /// Workspace is `None`; metadata is empty.
    #[must_use]
    pub fn new(
        user_id: impl Into<UserId>,
        agent_id: impl Into<AgentId>,
        session_id: impl Into<SessionId>,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            agent_id: agent_id.into(),
            session_id: session_id.into(),
            workspace: None,
            metadata: HashMap::new(),
        }
    }

    /// Builder-style: attach a workspace.
    #[must_use]
    pub fn with_workspace(mut self, workspace: impl Into<PathBuf>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// Builder-style: set a single metadata key.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Get a metadata value by key.
    #[must_use]
    pub fn meta(&self, key: &str) -> Option<&str> {
        self.metadata.get(key).map(String::as_str)
    }

    /// Cheaply wrap in `Arc` for shared ownership across tool copies.
    #[must_use]
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

/// Convenience constructors for `ToolContext` values used when the
/// caller has not supplied one. Kept in their own module so callers
/// don't have to scroll past struct definitions.
pub mod defaults {
    use sylvander_llm_anthropic::api::model::ModelInfo;
    use sylvander_protocol::types::{AgentId, SessionId, UserId};

    /// Sentinel user for system-originated actions.
    #[must_use]
    pub fn system_user() -> UserId {
        UserId::system()
    }

    /// Agent id derived from the model id (e.g. `model:claude-sonnet-5-20260601`).
    #[must_use]
    pub fn model_agent(model: &ModelInfo) -> AgentId {
        AgentId::new(format!("model:{}", model.id))
    }

    /// Session id for a no-session invocation (cron, internal task).
    #[must_use]
    pub fn ephemeral_session() -> SessionId {
        SessionId::new("__ephemeral__")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_required_fields() {
        let ctx = ToolContext::new("alice", "code-assistant", "sess-1");
        assert_eq!(ctx.user_id.0, "alice");
        assert_eq!(ctx.agent_id.0, "code-assistant");
        assert_eq!(ctx.session_id.0, "sess-1");
        assert!(ctx.workspace.is_none());
        assert!(ctx.metadata.is_empty());
    }

    #[test]
    fn builder_methods_chain() {
        let ctx = ToolContext::new("alice", "a", "s")
            .with_workspace("/home/alice/code")
            .with_metadata("channel", "telegram")
            .with_metadata("request_id", "req-42");

        assert_eq!(ctx.workspace.as_deref(), Some(std::path::Path::new("/home/alice/code")));
        assert_eq!(ctx.meta("channel"), Some("telegram"));
        assert_eq!(ctx.meta("request_id"), Some("req-42"));
        assert_eq!(ctx.meta("missing"), None);
    }

    #[test]
    fn system_user_id_sentinel_is_distinct() {
        let sys = UserId::system();
        let real = UserId::new("alice");
        assert_ne!(sys, real);
    }

    #[test]
    fn into_arc_gives_shared_ownership() {
        let ctx = ToolContext::new("alice", "a", "s").into_arc();
        let ctx2 = Arc::clone(&ctx);
        assert_eq!(ctx.user_id, ctx2.user_id);
    }

    #[test]
    fn clones_independently() {
        let ctx = ToolContext::new("alice", "a", "s").with_metadata("k", "v");
        let mut ctx2 = ctx.clone();
        ctx2.metadata.insert("k".into(), "v2".into());
        // Original is unaffected — clone is deep for the HashMap.
        assert_eq!(ctx.meta("k"), Some("v"));
        assert_eq!(ctx2.meta("k"), Some("v2"));
    }
}