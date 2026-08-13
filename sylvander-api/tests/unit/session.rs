use super::*;
fn effective_config_json() -> serde_json::Value {
    let source = serde_json::json!({ "kind": "agent_default" });
    serde_json::json!({
        "agent_id": "agent-1",
        "agent_revision": 3,
        "provider_id": "provider-1",
        "provider_revision": 7,
        "model_id": "model-1",
        "model_revision": 11,
        "reasoning_effort": "off",
        "permissions": {
            "file_access": "workspace_write",
            "network_access": "denied",
            "approval_policy": "allow"
        },
        "system_prompt_sha256": "digest",
        "prompt_manifest": {
            "layers": [],
            "aggregate_sha256": "aggregate",
            "total_bytes": 0
        },
        "execution_target": "local",
        "provenance": {
            "model": source.clone(),
            "reasoning_effort": source.clone(),
            "permissions": source.clone(),
            "prompt_profile": source.clone(),
            "system_prompt": source.clone(),
            "agent_workspace": source.clone(),
            "user_workspace": source.clone(),
            "execution_target": source
        }
    })
}

#[test]
fn effective_config_rejects_missing_revision_pins_and_prompt_manifest() {
    for field in ["provider_revision", "model_revision", "prompt_manifest"] {
        let mut json = effective_config_json();
        json.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<SessionEffectiveConfig>(json).is_err(),
            "missing {field} must fail closed"
        );
    }
}

#[test]
fn prompt_manifest_round_trips_in_composition_order() {
    let mut json = effective_config_json();
    json["prompt_manifest"] = serde_json::json!({
        "layers": [
            {
                "kind": "shared_safety",
                "reference": "safety-v2",
                "sha256": "aaa",
                "byte_count": 12
            },
            {
                "kind": "agent",
                "reference": "agent-1@3",
                "sha256": "bbb",
                "byte_count": 34
            },
            {
                "kind": "session_input",
                "sha256": "ccc",
                "byte_count": 5
            }
        ],
        "aggregate_sha256": "aggregate",
        "total_bytes": 51
    });

    let config: SessionEffectiveConfig = serde_json::from_value(json).unwrap();
    let manifest = &config.prompt_manifest;
    assert_eq!(manifest.layers[0].kind, PromptLayerKind::SharedSafety);
    assert_eq!(manifest.layers[1].kind, PromptLayerKind::Agent);
    assert_eq!(manifest.layers[2].kind, PromptLayerKind::SessionInput);
    assert_eq!(manifest.total_bytes, 51);
    let expected_manifest = manifest.clone();

    let round_trip: SessionEffectiveConfig =
        serde_json::from_value(serde_json::to_value(config).unwrap()).unwrap();
    assert_eq!(round_trip.prompt_manifest, expected_manifest);
}

#[test]
fn session_config_state_keeps_prompt_input_write_only() {
    let mut effective_json = effective_config_json();
    effective_json["prompt_manifest"] = serde_json::json!({
        "layers": [{
            "kind": "session_input",
            "reference": "session",
            "sha256": "session-digest",
            "byte_count": 24
        }],
        "aggregate_sha256": "aggregate",
        "total_bytes": 24
    });
    let state = SessionConfigState {
        session_id: SessionId::new("session-1"),
        revision: 2,
        overrides: SessionConfigOverrides {
            prompt_profile: Some("coding".into()),
            system_prompt: Some("private session sentinel".into()),
            ..SessionConfigOverrides::default()
        },
        effective: serde_json::from_value(effective_json).unwrap(),
    };
    let debug = format!("{:?}", state.overrides);
    assert!(!debug.contains("private session sentinel"));

    let encoded = serde_json::to_value(&state).unwrap();
    assert!(!encoded.to_string().contains("private session sentinel"));
    assert!(encoded["overrides"].get("system_prompt").is_none());
    assert_eq!(
        encoded["effective"]["prompt_manifest"]["layers"][0]["sha256"],
        "session-digest"
    );
    let decoded: SessionConfigState = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.overrides.prompt_profile.as_deref(), Some("coding"));
    assert!(decoded.overrides.system_prompt.is_none());
}

#[test]
fn pinned_effective_config_round_trips_and_validates() {
    let mut json = effective_config_json();
    json["provider_revision"] = serde_json::json!(7);
    json["model_revision"] = serde_json::json!(11);
    let config: SessionEffectiveConfig = serde_json::from_value(json).expect("pinned config");
    assert_eq!(
        config.require_revision_pins(),
        Ok(SessionRevisionPins {
            provider_revision: 7,
            model_revision: 11,
        })
    );
    let round_trip: SessionEffectiveConfig =
        serde_json::from_value(serde_json::to_value(&config).unwrap()).unwrap();
    assert_eq!(round_trip, config);
}

#[test]
fn revision_pin_validation_rejects_each_zero_value() {
    let mut json = effective_config_json();
    json["provider_revision"] = serde_json::json!(0);
    json["model_revision"] = serde_json::json!(1);
    let config: SessionEffectiveConfig = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(
        config.require_revision_pins(),
        Err(SessionRevisionPinError::ZeroProviderRevision)
    );

    json["provider_revision"] = serde_json::json!(1);
    json["model_revision"] = serde_json::json!(0);
    let config: SessionEffectiveConfig = serde_json::from_value(json).unwrap();
    assert_eq!(
        config.require_revision_pins(),
        Err(SessionRevisionPinError::ZeroModelRevision)
    );
}
#[test]
fn session_config_update_contract_preserves_optimistic_revision() {
    let request = SessionConfigUpdateRequest {
        session_id: SessionId::new("session-1"),
        expected_revision: 7,
        overrides: SessionConfigOverrides {
            model: Some(model("provider-b", "model-b")),
            reasoning_effort: Some(ReasoningEffort::High),
            ..SessionConfigOverrides::default()
        },
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["expected_revision"], 7);
    assert_eq!(json["overrides"]["model"]["provider_id"], "provider-b");
    assert_eq!(json["overrides"]["model"]["model_id"], "model-b");
    assert_eq!(
        serde_json::from_value::<SessionConfigUpdateRequest>(json).unwrap(),
        request
    );
}

#[test]
fn field_patch_preserves_omitted_write_only_values() {
    let mut overrides = SessionConfigOverrides {
        model: Some(model("provider-a", "model-a")),
        permissions: Some(PermissionProfile::default()),
        system_prompt: Some("private session sentinel".into()),
        ..SessionConfigOverrides::default()
    };
    let patch = SessionConfigPatch {
        model: Some(SessionConfigFieldPatch::Set {
            value: model("provider-b", "model-b"),
        }),
        permissions: Some(SessionConfigFieldPatch::Inherit),
        ..SessionConfigPatch::default()
    };
    let encoded = serde_json::to_value(&patch).unwrap();
    assert_eq!(encoded["model"]["operation"], "set");
    assert_eq!(encoded["permissions"]["operation"], "inherit");
    assert!(encoded.get("system_prompt").is_none());

    patch.apply_to(&mut overrides);
    assert_eq!(overrides.model, Some(model("provider-b", "model-b")));
    assert!(overrides.permissions.is_none());
    assert_eq!(
        overrides.system_prompt.as_deref(),
        Some("private session sentinel")
    );
}

fn model(provider_id: &str, model_id: &str) -> ModelSelection {
    ModelSelection {
        provider_id: provider_id.into(),
        model_id: model_id.into(),
    }
}
#[test]
fn current_override_resolves_only_an_exact_qualified_model() {
    let catalog = vec![model("anthropic", "shared"), model("openai", "gpt-5")];
    let current = SessionConfigOverrides {
        model: Some(model("openai", "gpt-5")),
        ..SessionConfigOverrides::default()
    };
    assert_eq!(
        current.resolve_model_selection(&catalog),
        Ok(Some(model("openai", "gpt-5")))
    );
    let missing = SessionConfigOverrides {
        model: Some(model("missing", "shared")),
        ..SessionConfigOverrides::default()
    };
    assert!(matches!(
        missing.resolve_model_selection(&catalog),
        Err(ModelSelectionResolutionError::Unavailable { .. })
    ));
}

#[test]
fn bare_model_id_override_is_rejected_as_an_unknown_field() {
    assert!(
        serde_json::from_value::<SessionConfigOverrides>(
            serde_json::json!({ "model_id": "shared" })
        )
        .is_err()
    );
}

#[test]
fn current_override_round_trips_a_qualified_model() {
    let overrides = SessionConfigOverrides {
        model: Some(model("openai", "gpt-5")),
        ..SessionConfigOverrides::default()
    };
    let json = serde_json::to_value(&overrides).unwrap();
    assert_eq!(json["model"]["provider_id"], "openai");
    assert!(json.get("model_id").is_none());
    assert_eq!(
        serde_json::from_value::<SessionConfigOverrides>(json).unwrap(),
        overrides
    );
}
