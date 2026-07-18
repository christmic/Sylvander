use super::*;
use sylvander_agent::bus::InProcessMessageBus;
use sylvander_agent::session_store::SqliteSessionStore;
use sylvander_agent::tools::InMemoryMemoryStore;
use sylvander_protocol::ModelSelection;

#[test]
fn capability_mapping_covers_the_canonical_vocabulary() {
    let model = ModelDefinition {
        provider_id: "provider".into(),
        model_id: "model".into(),
        revision: 1,
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: [
            "extended_thinking",
            "prompt_caching",
            "structured_output",
            "tool_use",
            "vision",
            "document_input",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        lifecycle: sylvander_protocol::ModelLifecycle::Active,
        pricing: None,
    };

    let (shadow, exact) = registry_model_capabilities(&model).unwrap();

    assert_eq!(
        shadow,
        ModelCapabilities::EXTENDED_THINKING
            | ModelCapabilities::PROMPT_CACHING
            | ModelCapabilities::STRUCTURED_OUTPUT
            | ModelCapabilities::TOOL_USE
            | ModelCapabilities::VISION
            | ModelCapabilities::DOCUMENT_INPUT
    );
    assert_eq!(
        exact,
        ProviderModelCapabilities::REASONING
            | ProviderModelCapabilities::PROMPT_CACHING
            | ProviderModelCapabilities::STRUCTURED_OUTPUT
            | ProviderModelCapabilities::TOOL_USE
            | ProviderModelCapabilities::VISION
            | ProviderModelCapabilities::DOCUMENT_INPUT
    );
}

#[test]
fn config_capability_mapping_uses_domain_aliases_and_fails_closed() {
    let mut model = ModelDefinitionConfig {
        id: "model".into(),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: vec!["reasoning".into()],
    };
    assert_eq!(
        model_capabilities(&model).unwrap(),
        ModelCapabilities::EXTENDED_THINKING
    );

    model.capabilities = vec!["telepathy".into()];
    assert!(matches!(
        model_capabilities(&model),
        Err(CompositionError::InvalidModelCapability {
            model,
            issue: ModelCapabilityIssue::Unknown
        }) if model == "model"
    ));

    let raw = "secret_future_capability";
    model.capabilities = vec![raw.into()];
    let error = model_capabilities(&model).unwrap_err();
    assert!(!error.to_string().contains(raw));
    assert!(!format!("{error:?}").contains(raw));
}

fn versioned_config() -> ServerConfig {
    ServerConfig::from_toml(
        r#"
schema_version = 1

[[model_providers]]
id = "alpha"
base_url = "https://alpha.invalid"
[model_providers.api_key]
source = "env"
name = "ALPHA_KEY"
[[model_providers.models]]
id = "shared"

[[model_providers]]
id = "beta"
base_url = "https://beta.invalid"
[model_providers.api_key]
source = "env"
name = "BETA_KEY"
[[model_providers.models]]
id = "shared"

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
"#,
    )
    .unwrap()
}

fn versioned_snapshot(config: &ServerConfig) -> VersionedRegistryCompositionSnapshot {
    let selection = |provider_id: &str| ModelSelection {
        provider_id: provider_id.into(),
        model_id: "shared".into(),
    };
    let model = |provider_id: &str, lifecycle| ModelDefinition {
        provider_id: provider_id.into(),
        model_id: "shared".into(),
        revision: if provider_id == "alpha" { 3 } else { 5 },
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: ["tool_use".into()].into(),
        lifecycle,
        pricing: None,
    };
    VersionedRegistryCompositionSnapshot {
        agent: config.agents[0].clone(),
        providers: BTreeMap::from([
            (
                "alpha".into(),
                ProviderDefinition {
                    id: "alpha".into(),
                    revision: 2,
                    kind: "anthropic_compatible".into(),
                    base_url: "https://alpha.invalid".into(),
                    credential_binding_id: "alpha-key".into(),
                },
            ),
            (
                "beta".into(),
                ProviderDefinition {
                    id: "beta".into(),
                    revision: 4,
                    kind: "anthropic_compatible".into(),
                    base_url: "https://beta.invalid".into(),
                    credential_binding_id: "beta-key".into(),
                },
            ),
        ]),
        models: BTreeMap::from([
            (
                selection("alpha"),
                model("alpha", sylvander_protocol::ModelLifecycle::Active),
            ),
            (
                selection("beta"),
                model(
                    "beta",
                    sylvander_protocol::ModelLifecycle::Deprecated { replacement: None },
                ),
            ),
        ]),
        default_model: selection("alpha"),
    }
}

#[tokio::test]
async fn versioned_builder_preserves_the_full_qualified_catalog() {
    let config = versioned_config();
    let directory = tempfile::tempdir().unwrap();
    let registry = crate::agent_registry::AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let bus: Arc<dyn MessageBus> = Arc::new(InProcessMessageBus::new());
    let sessions: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());

    let configured = build_registry_agent_versioned_with_resolver(
        &config,
        versioned_snapshot(&config),
        registry,
        bus,
        sessions,
        Arc::new(InMemoryMemoryStore::new()),
        None,
        Arc::new(crate::config::SystemSecretResolver),
        None,
        None,
    )
    .await
    .unwrap();
    let info = configured.run.runtime_model_info().await;

    assert_eq!(
        info.models
            .iter()
            .map(|model| (model.provider.as_str(), model.id.as_str()))
            .collect::<Vec<_>>(),
        vec![("alpha", "shared"), ("beta", "shared")]
    );
    assert!(matches!(
        info.models[1].lifecycle,
        sylvander_protocol::ModelLifecycle::Deprecated { .. }
    ));
    configured
        .run
        .select_qualified_model(
            ModelSelection {
                provider_id: "beta".into(),
                model_id: "shared".into(),
            },
            ReasoningEffort::Off,
        )
        .await
        .unwrap();

    let beta = resolve_session_config(
        &configured,
        &SessionConfigOverrides {
            model: Some(ModelSelection {
                provider_id: "beta".into(),
                model_id: "shared".into(),
            }),
            ..SessionConfigOverrides::default()
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(beta.provider_id, "beta");
    assert_eq!(beta.model_id, "shared");
    assert_eq!(beta.provider_revision, 4);
    assert_eq!(beta.model_revision, 5);

    assert!(matches!(
        resolve_session_config(
            &configured,
            &SessionConfigOverrides {
                model: Some(ModelSelection {
                    provider_id: "missing".into(),
                    model_id: "shared".into(),
                }),
                ..SessionConfigOverrides::default()
            },
            None,
            None,
        ),
        Err(CompositionError::ModelSelection(
            ModelSelectionResolutionError::Unavailable { provider_id, model_id }
        )) if provider_id == "missing" && model_id == "shared"
    ));
}

#[tokio::test]
async fn versioned_builder_preflights_every_model_before_router_construction() {
    let config = versioned_config();
    let mut snapshot = versioned_snapshot(&config);
    snapshot
        .models
        .get_mut(&ModelSelection {
            provider_id: "beta".into(),
            model_id: "shared".into(),
        })
        .unwrap()
        .capabilities = ["future_secret_capability".into()].into();
    let directory = tempfile::tempdir().unwrap();
    let registry = crate::agent_registry::AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let bus: Arc<dyn MessageBus> = Arc::new(InProcessMessageBus::new());
    let sessions: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());

    let result = build_registry_agent_versioned_with_resolver(
        &config,
        snapshot,
        registry,
        bus,
        sessions,
        Arc::new(InMemoryMemoryStore::new()),
        None,
        Arc::new(crate::config::SystemSecretResolver),
        None,
        None,
    )
    .await;
    let Err(error) = result else {
        panic!("unsupported model capability must fail before router construction");
    };

    assert!(matches!(
        error,
        CompositionError::ProviderFactory(message)
            if message == "model capability is unsupported by provider adapter"
    ));
}

#[test]
fn versioned_bindings_reject_a_partial_provider_closure() {
    let config = versioned_config();
    let mut snapshot = versioned_snapshot(&config);
    snapshot.providers.remove("beta");

    assert!(matches!(
        versioned_registry_revision_bindings(&snapshot.providers, &snapshot.models),
        Err(CompositionError::InvalidRegistryRevisionBinding)
    ));
}

#[tokio::test]
async fn configured_agent_uses_catalog_prompt_and_secret_reference() {
    let directory = tempfile::TempDir::new().unwrap();
    let secret_path = directory.path().join("provider.key");
    std::fs::write(&secret_path, "test-secret\n").unwrap();
    let input = format!(
        r#"
schema_version = 1

[[model_providers]]
id = "primary"
base_url = "https://models.example.test"

[model_providers.api_key]
source = "file"
path = "{}"

[[model_providers.models]]
id = "model-a"
context_window = 100000
max_output_tokens = 16000
capabilities = ["tool_use", "vision"]

[[execution_targets]]
id = "local"

[execution_targets.transport]
kind = "local"

[[agents]]
default_prompt_profile = "optimized"
allow_session_prompt = false

[agents.spec]
id = "assistant"
name = "Sylvander"

[agents.spec.model]
provider = "primary"
model_name = "model-a"

[[agents.prompt_profiles]]
id = "optimized"
qualified_models = [{{ provider_id = "primary", model_id = "model-a" }}]
system_prompt = "Optimized system prompt"

[[channels]]
id = "terminal"
default_agent = "assistant"

[channels.transport]
kind = "unix"
path = "/tmp/sylvander-test.sock"
"#,
        secret_path.display()
    );
    let mut config = ServerConfig::from_toml(&input).unwrap();
    let identity_reference = directory.path().join("ssh-identity.ref");
    std::fs::write(&identity_reference, "/tmp/sylvander-test-identity\n").unwrap();
    config
        .execution_targets
        .push(crate::config::ExecutionTargetConfig {
            id: "ssh:test".into(),
            transport: ExecutionTransportConfig::Ssh {
                host: "dev.example".into(),
                port: 22,
                user: "agent".into(),
                credential: crate::config::SecretRef::File {
                    path: identity_reference,
                },
                known_hosts: std::path::PathBuf::from("/tmp/sylvander-known-hosts"),
                control_path: std::path::PathBuf::from("/tmp/sylvander-ssh-control"),
                worktree_root: std::path::PathBuf::from("/tmp/sylvander-worktrees"),
            },
        });
    config.agents[0].agent_workspace = Some(crate::config::WorkspaceBindingConfig {
        execution_target: "local".into(),
        path: "/agent-home".into(),
        read_only: true,
        instruction_focus: None,
    });
    config.agents[0].workspace_mounts = vec![
        crate::config::WorkspaceMountConfig {
            reference: "shared-lib".into(),
            role: WorkspaceMountRole::Dependency,
            binding: crate::config::WorkspaceBindingConfig {
                execution_target: "local".into(),
                path: "/dependencies/shared-lib".into(),
                read_only: true,
                instruction_focus: None,
            },
            capabilities: WorkspaceCapabilityPolicy {
                read: true,
                write: false,
                command: false,
                git: true,
            },
        },
        crate::config::WorkspaceMountConfig {
            reference: "artifacts".into(),
            role: WorkspaceMountRole::Artifact,
            binding: crate::config::WorkspaceBindingConfig {
                execution_target: "local".into(),
                path: "/artifacts".into(),
                read_only: false,
                instruction_focus: None,
            },
            capabilities: WorkspaceCapabilityPolicy {
                read: true,
                write: true,
                command: false,
                git: false,
            },
        },
    ];
    let bus: Arc<dyn MessageBus> = Arc::new(InProcessMessageBus::new());
    let sessions: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());

    let mut agents = build_agents(
        &config,
        bus,
        sessions,
        Arc::new(InMemoryMemoryStore::new()),
        None,
        &crate::config::SystemSecretResolver,
    )
    .unwrap();

    assert_eq!(agents.len(), 1);
    assert_eq!(
        agents[0].spec.persona.system_prompt,
        format!(
            "{}\n\nOptimized system prompt",
            sylvander_agent::prompt::SHARED_SAFETY_PROMPT
        )
    );
    assert!(
        agents[0]
            .models
            .values()
            .next()
            .unwrap()
            .capabilities
            .contains(ModelCapabilities::TOOL_USE | ModelCapabilities::VISION)
    );

    let effective = resolve_session_config(
        &agents[0],
        &SessionConfigOverrides::default(),
        None,
        Some(std::path::Path::new("/work/project")),
    )
    .unwrap();
    assert_eq!(effective.model_id, "model-a");
    assert_eq!(effective.provider_revision, 1);
    assert_eq!(effective.model_revision, 1);
    assert_eq!(effective.prompt_profile.as_deref(), Some("optimized"));
    assert_eq!(effective.execution_target, "local");
    assert_eq!(
        effective
            .workspace_mounts
            .iter()
            .map(|mount| (mount.reference.as_str(), mount.role))
            .collect::<Vec<_>>(),
        vec![
            ("agent", WorkspaceMountRole::AgentHome),
            ("task", WorkspaceMountRole::Task),
            ("shared-lib", WorkspaceMountRole::Dependency),
            ("artifacts", WorkspaceMountRole::Artifact),
        ]
    );
    assert!(
        effective
            .workspace_mounts
            .iter()
            .find(|mount| mount.reference == "artifacts")
            .is_some_and(|mount| mount.capabilities.write)
    );
    assert_eq!(
        effective.user_workspace.unwrap().path,
        std::path::PathBuf::from("/work/project")
    );
    assert_eq!(
        effective.provenance.user_workspace.kind,
        SessionConfigSourceKind::RequestOverride
    );

    agents[0].definition.workspace_mounts[1].binding.path = "/dependencies/shared-lib".into();
    assert!(matches!(
        resolve_session_config(
            &agents[0],
            &SessionConfigOverrides::default(),
            None,
            Some(std::path::Path::new("/work/project")),
        ),
        Err(CompositionError::DuplicateWorkspaceMountLocation(reference))
            if reference == "artifacts"
    ));
    agents[0].definition.workspace_mounts[1].binding.path = "/artifacts".into();

    agents[0]
        .definition
        .workspace_mounts
        .push(crate::config::WorkspaceMountConfig {
            reference: "task".into(),
            role: WorkspaceMountRole::Dependency,
            binding: crate::config::WorkspaceBindingConfig {
                execution_target: "local".into(),
                path: "/collision".into(),
                read_only: true,
                instruction_focus: None,
            },
            capabilities: WorkspaceCapabilityPolicy::default(),
        });
    assert!(matches!(
        resolve_session_config(
            &agents[0],
            &SessionConfigOverrides::default(),
            None,
            Some(std::path::Path::new("/work/project")),
        ),
        Err(CompositionError::DuplicateWorkspaceMountReference(reference))
            if reference == "task"
    ));
    agents[0].definition.workspace_mounts.pop();
    assert_eq!(effective.system_prompt_sha256.len(), 64);
    assert!(!effective.prompt_manifest.layers.is_empty());

    agents[0].execution_targets.insert(
        "local".into(),
        ExecutionTransportConfig::Local {
            root: Some("/allowed".into()),
        },
    );
    let outside_root = resolve_session_config(
        &agents[0],
        &SessionConfigOverrides {
            user_workspace: Some(SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: "/other/project".into(),
                read_only: false,
                instruction_focus: None,
            }),
            ..SessionConfigOverrides::default()
        },
        None,
        None,
    );
    assert!(matches!(
        outside_root,
        Err(CompositionError::WorkspaceOutsideExecutionRoot { .. })
    ));
    agents[0].execution_targets.insert(
        "local".into(),
        ExecutionTransportConfig::Local { root: None },
    );

    let qualified = resolve_session_config(
        &agents[0],
        &SessionConfigOverrides {
            model: Some(ModelSelection {
                provider_id: "primary".into(),
                model_id: "model-a".into(),
            }),
            ..SessionConfigOverrides::default()
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        qualified.provenance.model.kind,
        SessionConfigSourceKind::SessionOverride
    );
    let channel_workspace = crate::config::WorkspaceBindingConfig {
        execution_target: "local".into(),
        path: "/channel/project".into(),
        read_only: true,
        instruction_focus: None,
    };
    let channel_effective = resolve_session_config(
        &agents[0],
        &SessionConfigOverrides::default(),
        Some(("terminal", &channel_workspace)),
        Some(std::path::Path::new("/request/project")),
    )
    .unwrap();
    assert_eq!(
        channel_effective.user_workspace.unwrap().path,
        std::path::PathBuf::from("/channel/project")
    );
    assert_eq!(
        channel_effective.provenance.user_workspace.kind,
        SessionConfigSourceKind::ChannelDefault
    );

    let error = resolve_session_config(
        &agents[0],
        &SessionConfigOverrides {
            system_prompt: Some("session prompt".into()),
            ..SessionConfigOverrides::default()
        },
        None,
        None,
    )
    .unwrap_err();
    assert!(matches!(error, CompositionError::SessionPromptDisabled));

    agents[0].definition.allow_session_prompt = true;
    for invalid in [
        String::new(),
        "x".repeat(sylvander_agent::prompt::MAX_SESSION_PROMPT_BYTES + 1),
        "private\0prompt".into(),
    ] {
        let error = resolve_session_config(
            &agents[0],
            &SessionConfigOverrides {
                system_prompt: Some(invalid.clone()),
                ..SessionConfigOverrides::default()
            },
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, CompositionError::InvalidPrompt));
        if !invalid.is_empty() {
            assert!(!error.to_string().contains(&invalid));
        }
    }
}
