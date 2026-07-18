use super::*;
use sylvander_protocol::{AgentAccessDraft, AgentId, AuthenticationMethod, ModelSelection};

fn model(provider_id: &str, model_id: &str) -> ModelSelection {
    ModelSelection {
        provider_id: provider_id.into(),
        model_id: model_id.into(),
    }
}

fn catalog() -> ServerConfig {
    ServerConfig::from_toml(
        r#"
schema_version = 1
[[model_providers]]
id = "primary"
base_url = "https://primary.invalid"
[model_providers.api_key]
source = "env"
name = "PRIMARY_TOKEN"
[[model_providers.models]]
id = "sonnet"

[[model_providers]]
id = "secondary"
base_url = "https://secondary.invalid"
[model_providers.api_key]
source = "env"
name = "SECONDARY_TOKEN"
[[model_providers.models]]
id = "sonnet"
"#,
    )
    .unwrap()
}

fn draft() -> AgentDefinitionDraft {
    AgentDefinitionDraft {
        agent_id: AgentId::new("oraculo"),
        revision: 2,
        name: "Oraculo".into(),
        description: "companion".into(),
        provider_id: "primary".into(),
        default_model_id: "sonnet".into(),
        allowed_models: vec![model("primary", "sonnet")],
        temperature: Some(0.2),
        max_tokens: Some(1024),
        system_prompt: "never reveal me".into(),
        tools: vec![AgentToolDraft::McpServer {
            name: "search".into(),
            command: "mcp-search".into(),
            args: vec!["serve".into()],
            environment: [(
                "TOKEN".into(),
                AgentSecretReference::Environment {
                    name: "SEARCH_TOKEN".into(),
                },
            )]
            .into_iter()
            .collect(),
        }],
        memory_stores: Vec::new(),
        ui_commands: Vec::new(),
        hooks: vec![AgentHookDraft {
            name: "lint".into(),
            command: "cargo check --quiet".into(),
            timeout_secs: 20,
            blocking: true,
        }],
        tool_presentations: vec![AgentToolPresentationDraft {
            tool_name: "search".into(),
            label: "Search docs".into(),
            kind: sylvander_protocol::ToolPresentationKind::Search,
            target_field: Some("query".into()),
        }],
        behavior: AgentBehaviorDraft::default(),
        agent_workspace: None,
        workspace_mounts: Vec::new(),
        prompt_profiles: Vec::new(),
        default_prompt_profile: None,
        allow_session_prompt: false,
        access: AgentAccessDraft::default(),
    }
}

#[test]
fn system_and_admin_role_are_privileged() {
    let mut user = AuthenticatedPrincipal::user("operator", AuthenticationMethod::Internal);
    assert!(!is_agent_administrator(Some(&user)));
    user.roles.push("admin".into());
    assert!(is_agent_administrator(Some(&user)));
    user.kind = PrincipalKind::System;
    user.roles.clear();
    assert!(is_agent_administrator(Some(&user)));
    assert!(!is_agent_administrator(None));
}

#[test]
fn definition_conversion_preserves_only_secret_references() {
    let config = definition_from_draft(draft()).unwrap();
    let encoded = match &config.spec.tools[0] {
        ToolRef::McpServer(server) => &server.envs["TOKEN"],
        ToolRef::Builtin { .. } => panic!("expected MCP server"),
    };
    assert!(encoded.starts_with(SECRET_REF_PREFIX));
    assert!(!encoded.contains("secret-value"));
    assert_eq!(draft_from_definition(&config).unwrap(), draft());
}

#[test]
fn legacy_inline_mcp_environment_value_fails_closed() {
    let mut config = definition_from_draft(draft()).unwrap();
    let ToolRef::McpServer(server) = &mut config.spec.tools[0] else {
        panic!("expected MCP server");
    };
    server.envs.insert("TOKEN".into(), "raw-secret".into());
    let error = draft_from_definition(&config).unwrap_err();
    assert_eq!(error.code, AgentAdminErrorCode::InvalidDefinition);
    assert!(!error.message.contains("raw-secret"));
}

#[test]
fn redaction_returns_hashes_and_counts_not_sensitive_values() {
    let config = definition_from_draft(draft()).unwrap();
    let view = redact_revision(&AgentRevision {
        definition: config,
        digest: "definition-digest".into(),
        created_at: 7,
        active: false,
    });
    let json = serde_json::to_string(&view).unwrap();
    assert!(!json.contains("never reveal me"));
    assert!(!json.contains("SEARCH_TOKEN"));
    assert!(!json.contains("mcp-search"));
    assert!(!json.contains("cargo check"));
    assert_eq!(view.definition.hooks[0].name, "lint");
    assert!(view.definition.hooks[0].blocking);
    assert_eq!(
        view.definition.system_prompt_sha256,
        digest("never reveal me")
    );
    assert_eq!(view.definition.allowed_models, draft().allowed_models);
}

#[test]
fn cross_provider_same_model_id_is_a_valid_exact_allowlist() {
    let mut candidate = draft();
    candidate.allowed_models.push(model("secondary", "sonnet"));
    let definition = definition_from_draft(candidate).unwrap();

    validate_against_catalog(&catalog(), &definition).unwrap();
    assert_eq!(definition.spec.model.allowed_models.len(), 2);
}

#[test]
fn qualified_prompt_profiles_round_trip_without_exposing_content() {
    let mut candidate = draft();
    candidate.prompt_profiles = vec![AgentPromptProfileDraft {
        id: "secondary-shared".into(),
        qualified_models: vec![model("secondary", "sonnet")],
        providers: Vec::new(),
        models: Vec::new(),
        system_prompt: "secondary private prompt".into(),
    }];
    let definition = definition_from_draft(candidate).unwrap();
    let revision = AgentRevision {
        definition: definition.clone(),
        digest: "definition-digest".into(),
        created_at: 7,
        active: false,
    };
    let view = redact_revision(&revision);
    assert_eq!(
        view.definition.prompt_profiles[0].qualified_models,
        vec![model("secondary", "sonnet")]
    );
    assert!(
        !serde_json::to_string(&view)
            .unwrap()
            .contains("secondary private prompt")
    );
    assert_eq!(
        draft_from_definition(&definition)
            .unwrap()
            .prompt_profiles
            .len(),
        1
    );

    let mut ambiguous = draft();
    ambiguous.prompt_profiles = vec![AgentPromptProfileDraft {
        id: "legacy-cross-product".into(),
        qualified_models: Vec::new(),
        providers: vec!["primary".into(), "secondary".into()],
        models: vec!["sonnet".into()],
        system_prompt: "private".into(),
    }];
    assert_eq!(
        definition_from_draft(ambiguous).unwrap_err().code,
        AgentAdminErrorCode::InvalidDefinition
    );
}

#[test]
fn allowed_models_must_be_non_empty_unique_and_include_the_default() {
    for allowed_models in [
        Vec::new(),
        vec![model("primary", "sonnet"), model("primary", "sonnet")],
        vec![model("secondary", "sonnet")],
        vec![model("", "sonnet"), model("primary", "sonnet")],
    ] {
        let mut candidate = draft();
        candidate.allowed_models = allowed_models;
        assert_eq!(
            definition_from_draft(candidate).unwrap_err().code,
            AgentAdminErrorCode::InvalidDefinition
        );
    }
}

#[test]
fn boot_catalog_rejects_unknown_allowed_provider_or_model() {
    for unknown in [model("missing", "sonnet"), model("secondary", "missing")] {
        let mut candidate = draft();
        candidate.allowed_models.push(unknown);
        let definition = definition_from_draft(candidate).unwrap();
        let mut boot_catalog = catalog();
        boot_catalog.agents.push(definition);
        assert!(boot_catalog.validate().is_err());
    }
}

#[test]
fn update_rejects_unknown_execution_target() {
    let mut definition = definition_from_draft(draft()).unwrap();
    definition.agent_workspace = Some(WorkspaceBindingConfig {
        execution_target: "missing-target".into(),
        path: "/workspace".into(),
        read_only: false,
        instruction_focus: None,
    });

    assert_eq!(
        validate_against_catalog(&catalog(), &definition)
            .unwrap_err()
            .code,
        AgentAdminErrorCode::InvalidDefinition
    );
}

#[test]
fn registry_storage_errors_do_not_expose_internal_details() {
    let response = map_registry_error(AgentRegistryError::Storage(
        "/private/db?token=secret-value".into(),
    ));
    assert_eq!(response.code, AgentAdminErrorCode::StorageUnavailable);
    assert!(!response.message.contains("secret-value"));
}

#[tokio::test]
async fn dynamic_qualified_update_is_a_plan_and_does_not_write_the_registry() {
    let catalog = ServerConfig::from_toml(
        r#"
schema_version = 1
[[model_providers]]
id = "primary"
base_url = "https://example.invalid"
[model_providers.api_key]
source = "env"
name = "TEST_TOKEN"
[[model_providers.models]]
id = "sonnet"
[[agents]]
revision = 1
[agents.spec]
id = "oraculo"
name = "Oraculo"
[agents.spec.model]
provider = "primary"
model_name = "sonnet"
"#,
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("agents.db"))
        .await
        .unwrap();
    let mut principal = AuthenticatedPrincipal::user("system", AuthenticationMethod::Internal);
    principal.kind = PrincipalKind::System;
    let mut candidate = draft();
    candidate.provider_id = "discovered-provider".into();
    candidate.default_model_id = "discovered-model".into();
    candidate.allowed_models = vec![model("discovered-provider", "discovered-model")];
    let dispatch = AgentAdminService::new(&registry, &catalog)
        .dispatch(
            Some(&principal),
            AgentAdminRequest::UpdateDefinition {
                expected_active_revision: 1,
                definition: Box::new(candidate),
            },
        )
        .await;
    assert!(matches!(
        dispatch,
        AgentAdminDispatch::Update {
            expected_active_revision: 1,
            ..
        }
    ));
    assert!(
        registry
            .inspect(&AgentId::new("oraculo"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn invalid_prompt_update_is_content_free_and_never_reaches_storage() {
    let catalog = catalog();
    let directory = tempfile::tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("agents.db"))
        .await
        .unwrap();
    let mut principal = AuthenticatedPrincipal::user("system", AuthenticationMethod::Internal);
    principal.kind = PrincipalKind::System;
    let mut candidate = draft();
    candidate.system_prompt = "private\0prompt".into();

    let dispatch = AgentAdminService::new(&registry, &catalog)
        .dispatch(
            Some(&principal),
            AgentAdminRequest::UpdateDefinition {
                expected_active_revision: 1,
                definition: Box::new(candidate),
            },
        )
        .await;
    let AgentAdminDispatch::Response(AgentAdminResponse::Error { error }) = dispatch else {
        panic!("invalid prompt must return a public error before staging");
    };
    assert_eq!(error.code, AgentAdminErrorCode::InvalidDefinition);
    assert!(error.message.contains("prompt configuration is invalid"));
    assert!(!error.message.contains("private"));
    assert!(
        registry
            .inspect(&AgentId::new("oraculo"))
            .await
            .unwrap()
            .is_empty()
    );
}
