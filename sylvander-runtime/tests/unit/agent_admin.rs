use super::*;
use sylvander_protocol::{
    AgentAccessDraft, AgentHookDraft, AgentId, AgentPromptProfileDraft, AgentUiCommandDraft,
    AuthenticationMethod, ModelSelection, SessionWorkspaceBinding,
};

pub(crate) fn draft_from_definition(
    definition: &AgentDefinitionConfig,
) -> Result<AgentDefinitionDraft, AgentAdminError> {
    let tools = definition
        .spec
        .tools
        .iter()
        .map(tool_to_draft)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AgentDefinitionDraft {
        agent_id: definition.spec.id.clone(),
        revision: definition.revision,
        name: definition.spec.name.clone(),
        description: definition.spec.persona.description.clone(),
        provider_id: definition.spec.model.provider.clone(),
        default_model_id: definition.spec.model.model_name.clone(),
        allowed_models: definition.spec.model.allowed_models.clone(),
        temperature: definition.spec.model.temperature,
        max_tokens: definition.spec.model.max_tokens,
        system_prompt: definition.spec.persona.system_prompt.clone(),
        tools,
        memory_stores: definition
            .spec
            .memory_stores
            .iter()
            .map(|store| {
                Ok(sylvander_protocol::AgentMemoryStoreDraft {
                    store_type: store.store_type.clone(),
                    path: store
                        .path
                        .to_str()
                        .ok_or_else(|| invalid_definition("memory store path must be valid UTF-8"))?
                        .to_owned(),
                })
            })
            .collect::<Result<_, AgentAdminError>>()?,
        ui_commands: definition
            .spec
            .ui_commands
            .iter()
            .map(command_to_draft)
            .collect(),
        hooks: definition
            .spec
            .hooks
            .iter()
            .map(|hook| AgentHookDraft {
                name: hook.name.clone(),
                phase: hook.phase,
                command: hook.command.clone(),
                timeout_secs: hook.timeout_secs,
                blocking: hook.blocking,
            })
            .collect(),
        tool_presentations: definition
            .spec
            .tool_presentations
            .iter()
            .map(|presentation| AgentToolPresentationDraft {
                tool_name: presentation.tool_name.clone(),
                label: presentation.label.clone(),
                kind: presentation.kind,
                target_field: presentation.target_field.clone(),
            })
            .collect(),
        behavior: AgentBehaviorDraft {
            max_iterations: definition.spec.behavior.max_iterations,
            max_retries: definition.spec.behavior.max_retries,
        },
        agent_workspace: definition.agent_workspace.as_ref().map(|workspace| {
            SessionWorkspaceBinding {
                execution_target: workspace.execution_target.clone(),
                path: PathBuf::from(&workspace.path),
                read_only: workspace.read_only,
                instruction_focus: workspace.instruction_focus.clone().map(Into::into),
            }
        }),
        workspace_mounts: definition
            .workspace_mounts
            .iter()
            .map(|mount| sylvander_protocol::SessionWorkspaceMount {
                reference: mount.reference.clone(),
                role: mount.role,
                binding: SessionWorkspaceBinding {
                    execution_target: mount.binding.execution_target.clone(),
                    path: PathBuf::from(&mount.binding.path),
                    read_only: mount.binding.read_only,
                    instruction_focus: mount.binding.instruction_focus.clone().map(Into::into),
                },
                capabilities: mount.capabilities,
            })
            .collect(),
        prompt_profiles: definition
            .prompt_profiles
            .iter()
            .map(profile_to_draft)
            .collect(),
        default_prompt_profile: definition.default_prompt_profile.clone(),
        allow_session_prompt: definition.allow_session_prompt,
        access: sylvander_protocol::AgentAccessDraft {
            allow_authenticated: definition.access.allow_authenticated,
            allowed_principals: definition.access.allowed_principals.clone(),
            allowed_roles: definition.access.allowed_roles.clone(),
        },
    })
}

fn decode_secret_reference(value: &str) -> Result<AgentSecretReference, AgentAdminError> {
    let encoded = value
        .strip_prefix(SECRET_REF_PREFIX)
        .ok_or_else(|| invalid_definition("legacy MCP environment values cannot be exported"))?;
    serde_json::from_str(encoded)
        .map_err(|_| invalid_definition("stored MCP secret reference is invalid"))
}

fn tool_to_draft(tool: &ToolRef) -> Result<AgentToolDraft, AgentAdminError> {
    match tool {
        ToolRef::Builtin { name } => Ok(AgentToolDraft::Builtin { name: name.clone() }),
        ToolRef::McpServer(server) => mcp_to_draft(server),
    }
}

fn mcp_to_draft(server: &McpServerConfig) -> Result<AgentToolDraft, AgentAdminError> {
    let environment = server
        .envs
        .iter()
        .map(|(name, value)| Ok((name.clone(), decode_secret_reference(value)?)))
        .collect::<Result<_, AgentAdminError>>()?;
    Ok(AgentToolDraft::McpServer {
        name: server.name.clone(),
        command: server.command.clone(),
        args: server.args.clone(),
        environment,
    })
}

fn command_to_draft(command: &UiCommandConfig) -> AgentUiCommandDraft {
    AgentUiCommandDraft {
        id: command.id.clone(),
        name: command.name.clone(),
        usage: command.usage.clone(),
        description: command.description.clone(),
        hint: command.hint.clone(),
        prompt: command.prompt.clone(),
    }
}

fn profile_to_draft(profile: &PromptProfileConfig) -> AgentPromptProfileDraft {
    AgentPromptProfileDraft {
        id: profile.id.clone(),
        qualified_models: profile.qualified_models.clone(),
        system_prompt: profile.system_prompt.clone(),
    }
}

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
            phase: sylvander_protocol::AgentHookPhase::BeforeTool,
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
    assert_eq!(
        view.definition.hooks[0].phase,
        sylvander_protocol::AgentHookPhase::BeforeTool
    );
    assert!(view.definition.hooks[0].blocking);
    assert_eq!(
        view.definition.system_prompt_sha256,
        digest("never reveal me")
    );
    assert_eq!(view.definition.allowed_models, draft().allowed_models);
}

#[test]
fn hook_identity_rejects_terminal_control_sequences() {
    let mut candidate = draft();
    candidate.hooks[0].name = "lint\u{1b}[31m".into();

    let error = definition_from_draft(candidate).unwrap_err();

    assert_eq!(error.code, AgentAdminErrorCode::InvalidDefinition);
    assert!(!error.message.contains('\u{1b}'));
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

    let legacy = serde_json::json!({
        "id": "legacy-cross-product",
        "qualified_models": [],
        "providers": ["primary", "secondary"],
        "models": ["sonnet"],
        "system_prompt": "private"
    });
    assert!(serde_json::from_value::<AgentPromptProfileDraft>(legacy).is_err());
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
allowed_models = [{ provider_id = "primary", model_id = "sonnet" }]
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
