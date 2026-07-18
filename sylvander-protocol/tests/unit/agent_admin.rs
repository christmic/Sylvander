use super::*;

#[test]
fn list_request_keeps_a_stable_default_page_size() {
    let request: AgentAdminRequest = serde_json::from_value(serde_json::json!({
        "operation": "list_revisions",
        "agent_id": "oraculo"
    }))
    .unwrap();
    assert_eq!(
        request,
        AgentAdminRequest::ListRevisions {
            agent_id: AgentId::new("oraculo"),
            before_revision: None,
            limit: 50,
        }
    );
}

#[test]
fn update_round_trip_carries_concurrency_and_secret_references_only() {
    let mut environment = BTreeMap::new();
    environment.insert(
        "TOKEN".into(),
        AgentSecretReference::Environment {
            name: "MCP_TOKEN".into(),
        },
    );
    let request = AgentAdminRequest::UpdateDefinition {
        expected_active_revision: 4,
        definition: Box::new(AgentDefinitionDraft {
            agent_id: AgentId::new("oraculo"),
            revision: 5,
            name: "Oraculo".into(),
            description: "companion".into(),
            provider_id: "anthropic".into(),
            default_model_id: "sonnet".into(),
            allowed_models: vec![
                ModelSelection {
                    provider_id: "anthropic".into(),
                    model_id: "sonnet".into(),
                },
                ModelSelection {
                    provider_id: "openai".into(),
                    model_id: "gpt-5".into(),
                },
            ],
            temperature: None,
            max_tokens: Some(32_000),
            system_prompt: "private prompt".into(),
            tools: vec![AgentToolDraft::McpServer {
                name: "search".into(),
                command: "mcp-search".into(),
                args: vec!["serve".into()],
                environment,
            }],
            memory_stores: Vec::new(),
            ui_commands: Vec::new(),
            hooks: Vec::new(),
            tool_presentations: Vec::new(),
            behavior: AgentBehaviorDraft::default(),
            agent_workspace: None,
            workspace_mounts: Vec::new(),
            prompt_profiles: Vec::new(),
            default_prompt_profile: None,
            allow_session_prompt: false,
            access: AgentAccessDraft::default(),
        }),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(json["expected_active_revision"], 4);
    assert_eq!(
        json["definition"]["tools"][0]["environment"]["TOKEN"]["name"],
        "MCP_TOKEN"
    );
    assert_eq!(
        serde_json::from_value::<AgentAdminRequest>(json).unwrap(),
        request
    );
}

#[test]
fn inspection_response_has_no_sensitive_definition_fields() {
    let response = AgentAdminResponse::Success {
        result: Box::new(AgentAdminResult::RevisionInspected {
            revision: AgentRevisionView {
                definition: RedactedAgentDefinition {
                    agent_id: AgentId::new("oraculo"),
                    revision: 5,
                    name: "Oraculo".into(),
                    description: "companion".into(),
                    provider_id: "anthropic".into(),
                    default_model_id: "sonnet".into(),
                    allowed_models: vec![ModelSelection {
                        provider_id: "openai".into(),
                        model_id: "gpt-5".into(),
                    }],
                    system_prompt_sha256: "abc".into(),
                    tools: vec![RedactedAgentTool::McpServer {
                        name: "search".into(),
                    }],
                    memory_store_types: vec!["sqlite".into()],
                    ui_commands: Vec::new(),
                    hooks: Vec::new(),
                    tool_presentations: Vec::new(),
                    behavior: AgentBehaviorDraft::default(),
                    agent_workspace_configured: true,
                    workspace_mount_count: 2,
                    prompt_profiles: Vec::new(),
                    default_prompt_profile: None,
                    allow_session_prompt: false,
                    access: RedactedAgentAccess {
                        allow_authenticated: false,
                        allowed_principal_count: 1,
                        allowed_roles: vec!["operator".into()],
                    },
                },
                digest_sha256: "def".into(),
                created_at_unix_secs: 1,
                active: true,
            },
        }),
    };
    let json = serde_json::to_string(&response).unwrap();
    assert!(
        json.contains("\"allowed_models\":[{\"provider_id\":\"openai\",\"model_id\":\"gpt-5\"}]")
    );
    for forbidden in [
        "system_prompt\"",
        "command\"",
        "args\"",
        "environment\"",
        "workspace\"",
        "allowed_principals",
        "secret",
    ] {
        assert!(!json.contains(forbidden), "inspection leaked {forbidden}");
    }
}

#[test]
fn legacy_definition_without_allowed_models_defaults_to_empty() {
    let definition: AgentDefinitionDraft = serde_json::from_value(serde_json::json!({
        "agent_id": "oraculo",
        "revision": 1,
        "name": "Oraculo",
        "provider_id": "anthropic",
        "default_model_id": "sonnet"
    }))
    .unwrap();

    assert!(definition.allowed_models.is_empty());
}

#[test]
fn write_draft_debug_redacts_raw_prompts() {
    let profile = AgentPromptProfileDraft {
        id: "private-profile".into(),
        qualified_models: vec![ModelSelection {
            provider_id: "provider-1".into(),
            model_id: "model-1".into(),
        }],
        providers: Vec::new(),
        models: Vec::new(),
        system_prompt: "profile prompt must never reach logs".into(),
    };
    let profile_debug = format!("{profile:?}");
    assert!(profile_debug.contains("[REDACTED]"));
    assert!(profile_debug.contains("qualified_model_count"));
    assert!(!profile_debug.contains("profile prompt must never reach logs"));

    let definition: AgentDefinitionDraft = serde_json::from_value(serde_json::json!({
        "agent_id": "oraculo",
        "revision": 1,
        "name": "Oraculo",
        "provider_id": "provider-1",
        "default_model_id": "model-1",
        "system_prompt": "definition prompt must never reach logs",
        "prompt_profiles": [profile]
    }))
    .unwrap();
    let definition_debug = format!("{definition:?}");
    assert!(definition_debug.contains("[REDACTED]"));
    assert!(!definition_debug.contains("definition prompt must never reach logs"));
    assert!(!definition_debug.contains("profile prompt must never reach logs"));
}

#[test]
fn qualified_allowed_models_round_trip_and_appear_in_schema() {
    let definition: AgentDefinitionDraft = serde_json::from_value(serde_json::json!({
        "agent_id": "oraculo",
        "revision": 2,
        "name": "Oraculo",
        "provider_id": "anthropic",
        "default_model_id": "sonnet",
        "allowed_models": [
            { "provider_id": "anthropic", "model_id": "sonnet" },
            { "provider_id": "openai", "model_id": "gpt-5" }
        ]
    }))
    .unwrap();

    assert_eq!(
        serde_json::from_value::<AgentDefinitionDraft>(serde_json::to_value(&definition).unwrap())
            .unwrap(),
        definition
    );
    assert_eq!(definition.allowed_models[1].provider_id, "openai");
    assert_eq!(definition.allowed_models[1].model_id, "gpt-5");

    let schema = serde_json::to_string(&schemars::schema_for!(AgentDefinitionDraft)).unwrap();
    assert!(schema.contains("allowed_models"));
    assert!(schema.contains("ModelSelection"));
}

#[test]
fn conflict_error_preserves_machine_readable_revisions() {
    let response: AgentAdminResponse = serde_json::from_value(serde_json::json!({
        "status": "error",
        "error": {
            "code": "revision_conflict",
            "message": "active revision changed",
            "agent_id": "oraculo",
            "expected_active_revision": 4,
            "actual_active_revision": 5
        }
    }))
    .unwrap();
    assert!(matches!(
        response,
        AgentAdminResponse::Error {
            error: AgentAdminError {
                code: AgentAdminErrorCode::RevisionConflict,
                expected_active_revision: Some(4),
                actual_active_revision: Some(5),
                ..
            }
        }
    ));
}
