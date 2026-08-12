use super::*;
use sylvander_protocol::types::{AgentId, UserId};

fn execution() -> AgentExecutionContext {
    AgentExecutionContext::restricted_for("alice", "code-assistant", "sess-1")
}

#[test]
fn new_wraps_execution_context_in_arc() {
    let ctx = ToolContext::new(execution());
    assert_eq!(ctx.user_id(), "alice");
    assert_eq!(ctx.agent_id(), "code-assistant");
    assert_eq!(ctx.session_id(), "sess-1");
    assert!(ctx.surface.fs_root.is_none());
    assert!(ctx.surface.capabilities.is_empty());
    assert!(matches!(
        ctx.memory_context().relationship_owner(),
        Err(crate::tools::memory::MemoryStoreError::AccessDenied)
    ));
}

#[test]
fn application_context_issues_memory_authority() {
    let ctx = ToolContext::for_runtime(execution());
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
    let ctx = ToolContext::new(execution())
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
    let ctx = ToolContext::new(execution());
    assert!(ctx.budget.timeout.is_some());
    assert_eq!(ctx.budget.max_retries, 0);
}

#[test]
fn host_allowed_respects_policy() {
    let mut ctx = ToolContext::new(execution());
    assert!(!ctx.host_allowed("api.example.com"));

    ctx.surface.network = NetworkPolicy::All;
    assert!(ctx.host_allowed("api.example.com"));

    ctx.surface.network = NetworkPolicy::Allow(vec!["api.openai.com".into()]);
    assert!(ctx.host_allowed("api.openai.com"));
    assert!(!ctx.host_allowed("evil.example.com"));
}

#[test]
fn clones_independently_but_share_execution_arc() {
    let ctx = ToolContext::new(execution());
    let ctx2 = ctx.clone();
    // Execution authority is immutable and shared across cloned tool views.
    assert!(Arc::ptr_eq(&ctx.execution, &ctx2.execution));
}

#[test]
fn system_sentinel_is_distinct() {
    let real = ToolContext::new(execution());
    let sys = defaults::system_tool_context();
    assert_ne!(real.user_id(), sys.user_id());
}
