use super::*;
#[test]
fn reasoning_effort_has_stable_provider_neutral_budgets() {
    assert_eq!(ReasoningEffort::Off.budget_tokens(), None);
    assert_eq!(ReasoningEffort::Low.budget_tokens(), Some(2_048));
    assert_eq!(ReasoningEffort::Medium.budget_tokens(), Some(8_192));
    assert_eq!(ReasoningEffort::High.budget_tokens(), Some(20_000));
}

#[test]
fn model_descriptors_require_current_capability_and_lifecycle_metadata() {
    assert!(
        serde_json::from_value::<ModelDescriptor>(serde_json::json!({
            "id": "model-a",
            "provider": "test",
            "capabilities": 0,
            "reasoning_efforts": ["off"]
        }))
        .is_err()
    );
}

#[test]
fn model_capability_names_are_canonical_and_strict() {
    let descriptor: ModelDescriptor = serde_json::from_value(serde_json::json!({
        "id": "model-a",
        "provider": "test",
        "capabilities": 8,
        "capability_names": ["tool_use", "vision"],
        "reasoning_efforts": ["off"],
        "lifecycle": {"status": "active"}
    }))
    .expect("canonical capability names");
    assert_eq!(
        descriptor.capability_names,
        [ModelCapability::ToolUse, ModelCapability::Vision]
    );
    assert!(
        serde_json::from_value::<ModelDescriptor>(serde_json::json!({
            "id": "model-a",
            "provider": "test",
            "capabilities": 0,
            "capability_names": ["telepathy"],
            "reasoning_efforts": ["off"],
            "lifecycle": {"status": "active"}
        }))
        .is_err()
    );
}
fn model(provider_id: &str, model_id: &str) -> ModelSelection {
    ModelSelection {
        provider_id: provider_id.into(),
        model_id: model_id.into(),
    }
}

#[test]
fn qualified_model_selection_has_a_stable_schema_and_wire_shape() {
    let selection = model("anthropic", "claude-sonnet");
    assert_eq!(
        serde_json::to_value(&selection).unwrap(),
        serde_json::json!({
            "provider_id": "anthropic",
            "model_id": "claude-sonnet"
        })
    );

    let schema = serde_json::to_value(schemars::schema_for!(ModelSelection)).unwrap();
    assert_eq!(
        schema["required"],
        serde_json::json!(["provider_id", "model_id"])
    );
    assert!(schema["properties"]["provider_id"].is_object());
    assert!(schema["properties"]["model_id"].is_object());
}
