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
