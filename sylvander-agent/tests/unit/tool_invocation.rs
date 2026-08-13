use serde_json::json;

use super::*;

fn descriptor(name: &str, class: ToolInvocationClass) -> ToolInvocationDescriptor {
    ToolInvocationDescriptor {
        name: name.into(),
        class,
        recovery_policy: ToolRecoveryPolicy::NeverReplay,
        input_schema: json!({"type": "object"}),
    }
}

#[test]
fn turn_snapshot_distinguishes_executable_routes_from_prompt_only_skills() {
    let base = ToolInvocationSnapshot::from_descriptors(&[descriptor(
        "command",
        ToolInvocationClass::Terminal,
    )]);
    let turn = base.for_turn("sha256:tools", ["review-guidelines".into()]);

    assert_ne!(base.revision(), turn.revision());
    assert!(turn.features().contains(&CapabilityFeature {
        name: "command".into(),
        kind: CapabilityFeatureKind::Executable(
            ToolInvocationClass::Terminal,
            ToolRecoveryPolicy::NeverReplay,
        ),
    }));
    assert!(turn.features().contains(&CapabilityFeature {
        name: "review-guidelines".into(),
        kind: CapabilityFeatureKind::PromptContext,
    }));
    assert!(base.has_same_executable_surface(&turn));
    assert!(!turn.authorizes(
        "review-guidelines",
        ToolInvocationClass::Extension,
        ToolRecoveryPolicy::NeverReplay,
    ));

    let forged = ToolInvocationSnapshot::from_descriptors(&[
        descriptor("command", ToolInvocationClass::Terminal),
        descriptor("browser", ToolInvocationClass::Browser),
    ]);
    assert!(!base.has_same_executable_surface(&forged));
}

#[tokio::test]
async fn standalone_gateway_rejects_unknown_route_and_forged_owner_input() {
    let gateway =
        RegistryBoundToolGateway::new(vec![descriptor("command", ToolInvocationClass::Terminal)]);
    let context = crate::tool_context::ToolContext::new(
        crate::execution_context::AgentExecutionContext::restricted_for(
            "alice",
            "agent-a",
            "session-a",
        ),
    );
    let snapshot = gateway.snapshot();

    let unknown = ToolInvocationRequest::new(
        ToolInvocationIdentity::new("00000000-0000-4000-8000-000000000001", "call-1"),
        "browser",
        None,
        None,
        &context,
        json!({}),
        snapshot.clone(),
    );
    assert!(matches!(
        gateway.authorize(unknown).await,
        Err(ToolInvocationError::Unavailable)
    ));

    let forged = ToolInvocationRequest::new(
        ToolInvocationIdentity::new("00000000-0000-4000-8000-000000000002", "call-2"),
        "command",
        Some(ToolInvocationClass::Terminal),
        Some(ToolRecoveryPolicy::NeverReplay),
        &context,
        json!({"metadata": {"user_id": "mallory"}}),
        snapshot,
    );
    assert!(matches!(
        gateway.authorize(forged).await,
        Err(ToolInvocationError::AccessDenied)
    ));
}

#[tokio::test]
async fn recovery_policy_is_independent_and_authorized_exactly() {
    let mut retryable = descriptor("lookup", ToolInvocationClass::Read);
    retryable.recovery_policy = ToolRecoveryPolicy::RetryWithSameInvocation;
    let never = descriptor("lookup", ToolInvocationClass::Read);
    let retry_snapshot = ToolInvocationSnapshot::from_descriptors(&[retryable.clone()]);
    let never_snapshot = ToolInvocationSnapshot::from_descriptors(&[never]);
    assert_ne!(retry_snapshot.revision(), never_snapshot.revision());

    let gateway = RegistryBoundToolGateway::new(vec![retryable]);
    let context = crate::tool_context::ToolContext::new(
        crate::execution_context::AgentExecutionContext::restricted_for(
            "alice",
            "agent-a",
            "session-a",
        ),
    );
    let forged_policy = ToolInvocationRequest::new(
        ToolInvocationIdentity::new("00000000-0000-4000-8000-000000000003", "call-3"),
        "lookup",
        Some(ToolInvocationClass::Read),
        Some(ToolRecoveryPolicy::NeverReplay),
        &context,
        json!({}),
        gateway.snapshot(),
    );
    assert!(matches!(
        gateway.authorize(forged_policy).await,
        Err(ToolInvocationError::AccessDenied)
    ));
}
