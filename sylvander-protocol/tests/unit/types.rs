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
fn user_id_round_trips() {
    let u: UserId = "alice".into();
    assert_eq!(u.0, "alice");
    let u2: UserId = String::from("bob").into();
    assert_eq!(u2.0, "bob");
    assert_eq!(u.to_string(), "alice");
}

#[test]
fn user_id_system_sentinel_is_distinct() {
    let sys = UserId::system();
    let real = UserId::new("alice");
    assert_ne!(sys, real);
    assert_ne!(sys.0, "alice");
}

#[test]
fn user_id_serializes_as_inner_string() {
    let u = UserId::new("alice");
    let json = serde_json::to_string(&u).unwrap();
    assert_eq!(json, "\"alice\"");
}

#[test]
fn three_id_types_share_a_constructor_pattern() {
    // Smoke: AgentId / SessionId / UserId all have the same shape.
    let _a: AgentId = "a".into();
    let _s: SessionId = "s".into();
    let _u: UserId = "u".into();
}

#[test]
fn current_bus_messages_may_omit_an_empty_attachment_list() {
    let mut value =
        serde_json::to_value(BusMessage::user_chat("s".into(), "u", "hi")).expect("serialize");
    value.as_object_mut().unwrap().remove("attachments");
    let message: BusMessage = serde_json::from_value(value).expect("current optional field");
    assert!(message.attachments.is_empty());
}

#[test]
fn reasoning_effort_has_stable_provider_neutral_budgets() {
    assert_eq!(ReasoningEffort::Off.budget_tokens(), None);
    assert_eq!(ReasoningEffort::Low.budget_tokens(), Some(2_048));
    assert_eq!(ReasoningEffort::Medium.budget_tokens(), Some(8_192));
    assert_eq!(ReasoningEffort::High.budget_tokens(), Some(20_000));
}

#[test]
fn approval_messages_require_explicit_scope_contracts() {
    assert!(
        serde_json::from_value::<SystemMessage>(serde_json::json!({
            "type": "approve_tool",
            "call_id": "call-1",
            "approved": true
        }))
        .is_err()
    );

    assert!(
        serde_json::from_value::<StreamEvent>(serde_json::json!({
            "type": "tool_approval_required",
            "batch_id": "batch-1",
            "tools": []
        }))
        .is_err()
    );
}

#[test]
fn approval_rejection_reason_round_trips_without_transport_semantics() {
    let system = SystemMessage::ApproveTool {
        call_id: "call-1".into(),
        approved: false,
        scope: ApprovalScope::Once,
        reason: Some("unsafe outside workspace".into()),
    };
    let json = serde_json::to_value(&system).expect("serialize approval");
    let decoded: SystemMessage = serde_json::from_value(json).expect("decode approval");
    assert_eq!(decoded, system);
}

#[test]
fn retry_events_require_an_explicit_typed_cause() {
    assert!(
        serde_json::from_value::<StreamEvent>(serde_json::json!({
            "type": "model_retry",
            "attempt": 1,
            "max_attempts": 3,
            "delay_ms": 100,
            "reason": "temporary"
        }))
        .is_err()
    );
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

#[test]
fn platform_snapshot_round_trip_keeps_status_semantic() {
    let snapshot = PlatformSnapshot {
        features: vec![PlatformFeature {
            kind: PlatformFeatureKind::Mcp,
            name: "code search".into(),
            status: PlatformFeatureStatus::Configured,
            summary: "configured".into(),
            source: Some("search-mcp".into()),
            trust: Some(PlatformTrust::External),
            auth: PlatformAuthStatus::Configured,
            capabilities: vec!["tools".into()],
            reloadable: false,
        }],
        commands: vec![UiCommandDescriptor {
            id: "review-security".into(),
            name: "security-review".into(),
            usage: "/security-review [scope]".into(),
            description: "Review a selected scope".into(),
            hint: "workspace command".into(),
            source: "agent configuration".into(),
            trust: PlatformTrust::Workspace,
            effect: UiCommandEffect::SubmitPrompt {
                template: "Review {{args}} for security issues.".into(),
            },
        }],
        tool_presentations: vec![ToolPresentationDescriptor {
            tool_name: "search".into(),
            label: "Search".into(),
            kind: ToolPresentationKind::Search,
            target_field: Some("query".into()),
            source: "agent configuration".into(),
            trust: PlatformTrust::Workspace,
        }],
    };
    let json = serde_json::to_string(&snapshot).unwrap();
    let restored: PlatformSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, snapshot);
}

#[test]
fn ui_protocol_accepts_only_the_current_revision() {
    let current = UiProtocolHello {
        client_name: "test".into(),
        min_version: UI_PROTOCOL_VERSION,
        max_version: UI_PROTOCOL_VERSION,
        capabilities: vec!["diagnostics".into()],
    };
    assert_eq!(negotiate_ui_protocol(&current), Ok(UI_PROTOCOL_VERSION));

    for version in [UI_PROTOCOL_VERSION - 1, UI_PROTOCOL_VERSION + 1] {
        let incompatible = UiProtocolHello {
            min_version: version,
            max_version: version,
            ..current.clone()
        };
        let error = negotiate_ui_protocol(&incompatible).expect_err("must reject");
        assert_eq!(error.code, "incompatible_protocol");
        assert_eq!(error.server_min_version, UI_PROTOCOL_VERSION);
        assert_eq!(error.server_max_version, UI_PROTOCOL_VERSION);
    }
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

#[test]
fn feedback_requires_an_opaque_target_and_has_stable_wire_values() {
    let feedback = RunFeedback {
        target: FeedbackTarget("sha256:opaque".into()),
        rating: FeedbackRating::Negative,
        note: Some("tool changed the wrong file".into()),
        correction: Some("edit src/api.rs instead".into()),
        tags: vec!["correctness".into()],
        task_result: Some(FeedbackTaskResult::Failed),
        artifacts: vec![EvidenceReference {
            locator: "worktree:session-1".into(),
            digest_sha256: None,
        }],
        validations: vec![EvidenceReference {
            locator: "test:cargo-test".into(),
            digest_sha256: Some("a".repeat(64)),
        }],
        privacy_class: FeedbackPrivacyClass::Private,
    };
    let json = serde_json::to_value(&feedback).unwrap();
    assert_eq!(json["rating"], "negative");
    assert_eq!(json["target"], "sha256:opaque");
    assert!(json.get("run_id").is_none());
    assert!(json.get("turn_id").is_none());
    assert_eq!(
        serde_json::from_value::<RunFeedback>(json).unwrap(),
        feedback
    );
}

#[test]
fn feedback_target_accepts_only_the_server_digest_shape() {
    assert!(FeedbackTarget(format!("sha256:{}", "a".repeat(64))).is_well_formed());
    for invalid in [
        format!("sha256:{}", "a".repeat(63)),
        format!("sha256:{}", "A".repeat(64)),
        format!("sha256:{}", "g".repeat(64)),
        "sha256:opaque".into(),
        format!("sha512:{}", "a".repeat(64)),
    ] {
        assert!(
            !FeedbackTarget(invalid.clone()).is_well_formed(),
            "{invalid} must not be accepted as a server-issued target"
        );
    }
}

#[test]
fn terminal_error_has_a_stable_typed_wire_shape() {
    let event = StreamEvent::Error {
        message: "provider unavailable".into(),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "error");
    assert_eq!(json["message"], "provider unavailable");
    assert_eq!(serde_json::from_value::<StreamEvent>(json).unwrap(), event);
}
