use std::time::Duration;

use super::{AgentExecutionContext, ExecutionActor, ExecutionCapability, ExecutionWorkspace};

#[test]
fn execution_authority_is_explicit_and_restricted_by_default() {
    let context = AgentExecutionContext::restricted(ExecutionActor::new("u", "a", "s"))
        .with_workspace(ExecutionWorkspace {
            workspace_id: "project".into(),
            target_id: "workspace".into(),
            read_only: true,
        })
        .with_capability(ExecutionCapability::WorkspaceRead)
        .with_timeout(Duration::from_secs(30))
        .with_trace_id("trace");

    assert_eq!(context.actor.session_id, "s");
    assert!(
        context
            .capabilities
            .contains(&ExecutionCapability::WorkspaceRead)
    );
    assert!(!context.capabilities.contains(&ExecutionCapability::Process));
    assert_eq!(context.timeout, Some(Duration::from_secs(30)));
}
