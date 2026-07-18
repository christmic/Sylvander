use serde_json::json;

use super::*;

fn descriptor(name: &str, class: ToolInvocationClass) -> ToolInvocationDescriptor {
    ToolInvocationDescriptor {
        name: name.into(),
        class,
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
        kind: CapabilityFeatureKind::Executable(ToolInvocationClass::Terminal),
    }));
    assert!(turn.features().contains(&CapabilityFeature {
        name: "review-guidelines".into(),
        kind: CapabilityFeatureKind::PromptContext,
    }));
    assert!(base.has_same_executable_surface(&turn));
    assert!(!turn.authorizes("review-guidelines", ToolInvocationClass::Extension));

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
    let context = crate::tool_context::ToolContext::new(sylvander_protocol::SessionContext::new(
        "alice",
        "agent-a",
        "session-a",
    ));
    let snapshot = gateway.snapshot();

    let unknown = ToolInvocationRequest::new(
        "call-1",
        "browser",
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
        "call-2",
        "command",
        Some(ToolInvocationClass::Terminal),
        &context,
        json!({"metadata": {"user_id": "mallory"}}),
        snapshot,
    );
    assert!(matches!(
        gateway.authorize(forged).await,
        Err(ToolInvocationError::AccessDenied)
    ));
}
