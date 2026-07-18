use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::json;
use sylvander_agent::bus::{InProcessMessageBus, MessageBus};
use sylvander_agent::session_store::{
    SessionLifetime, SessionStore, SqliteSessionStore, StoredSession,
};
use sylvander_agent::tools::InMemoryMemoryStore;
use sylvander_protocol::{
    AgentId, AuthenticatedPrincipal, AuthenticationMethod, BoundaryContext, BusMessage,
    SessionConfigOverrides, SessionConfigUpdateRequest, SessionCreateRequest, SessionMetadata,
};
use tempfile::tempdir;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::Runtime;
use crate::agent_registry::AgentRegistry;
use crate::agent_registry_snapshot::AgentSnapshotSelection;
use crate::agent_registry_snapshot_v3::AgentSnapshotSelectionV3;
use crate::config::{
    ExecutionTargetConfig, ExecutionTransportConfig, SecretRef, ServerConfig, SystemSecretResolver,
    WorkspaceBindingConfig,
};
use crate::registry_domain::CredentialBindingRevision;

const TEXT_STREAM: &str = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"model-a\",\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}

event: content_block_start
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}

event: content_block_delta
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}

event: content_block_stop
data: {\"type\":\"content_block_stop\",\"index\":0}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

fn config(base_url: &str, secret_path: &std::path::Path) -> ServerConfig {
    let mut config = ServerConfig::from_toml(&format!(
        r#"
schema_version = 1

[[model_providers]]
id = "alpha"
base_url = "{base_url}"

[model_providers.api_key]
source = "file"
path = "{}"

[[model_providers.models]]
id = "model-a"
context_window = 100000
max_output_tokens = 4096
capabilities = ["tool_use", "prompt_caching"]

[[model_providers.models]]
id = "model-b"
context_window = 80000
max_output_tokens = 2048
capabilities = ["tool_use"]

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "model-a"
"#,
        secret_path.display()
    ))
    .unwrap();
    crate::configure_test_memory_integrity(&mut config, secret_path.parent().unwrap(), secret_path);
    config
}

fn dual_provider_config(
    alpha_url: &str,
    alpha_secret: &std::path::Path,
    beta_url: &str,
    beta_secret: &std::path::Path,
) -> ServerConfig {
    let mut config = ServerConfig::from_toml(&format!(
        r#"
schema_version = 1

[[model_providers]]
id = "alpha"
base_url = "{alpha_url}"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "shared"
context_window = 100000
max_output_tokens = 4096
capabilities = ["tool_use"]

[[model_providers]]
id = "beta"
base_url = "{beta_url}"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "shared"
context_window = 100000
max_output_tokens = 4096
capabilities = ["tool_use"]

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
"#,
        alpha_secret.display(),
        beta_secret.display()
    ))
    .unwrap();
    config.agents[0].spec.model.allowed_models = vec![
        ModelSelection {
            provider_id: "alpha".into(),
            model_id: "shared".into(),
        },
        ModelSelection {
            provider_id: "beta".into(),
            model_id: "shared".into(),
        },
    ];
    config
}

async fn expect_request(server: &MockServer, key: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", format!("Bearer {key}")))
        .and(body_partial_json(json!({"model": "model-a"})))
        .respond_with(ResponseTemplate::new(200).set_body_raw(TEXT_STREAM, "text/event-stream"))
        .expect(1)
        .mount(server)
        .await;
}

async fn persisted_session(
    agent: &ConfiguredAgent,
    store: &SqliteSessionStore,
    name: &str,
) -> sylvander_agent::spec::SessionId {
    persisted_session_with_overrides(agent, store, name, SessionConfigOverrides::default()).await
}

async fn persisted_session_with_overrides(
    agent: &ConfiguredAgent,
    store: &SqliteSessionStore,
    name: &str,
    overrides: SessionConfigOverrides,
) -> sylvander_agent::spec::SessionId {
    let metadata = SessionMetadata {
        workspace: std::path::PathBuf::from("/workspace"),
        name: name.into(),
        user_id: "user".into(),
    };
    let session_id = sylvander_protocol::SessionId::new(uuid::Uuid::new_v4().to_string());
    agent
        .attach_authenticated_session(session_id.clone(), metadata.clone())
        .await
        .expect("attach authenticated session");
    let mut stored = StoredSession::new(
        session_id.clone(),
        name,
        SessionLifetime::Persistent,
        metadata,
        vec![agent.spec.id.clone()],
    );
    stored.effective_config = Some(resolve_session_config(agent, &overrides, None, None).unwrap());
    store.save(&stored).await.unwrap();
    session_id
}

#[cfg(unix)]
#[tokio::test]
async fn configured_sandbox_executor_supplies_workspace_context_to_the_agent() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempdir().unwrap();
    let secret = directory.path().join("secret");
    std::fs::write(&secret, "test-key\n").unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    for path in [".agents/skills", ".sylvander/skills", "skills"] {
        std::fs::create_dir_all(workspace.join(path)).unwrap();
    }
    std::fs::write(
        workspace.join("AGENTS.md"),
        "sandbox-injected-workspace-instruction",
    )
    .unwrap();
    let arguments = directory.path().join("arguments");
    let runtime = directory.path().join("runtime");
    std::fs::write(
        &runtime,
        format!(
            r#"#!/bin/sh
printf '%s\0' "$@" > '{}'
[ "$1" = rm ] && exit 0
[ "$1" = run ] || exit 90
shift
while [ "$#" -gt 0 ]; do
  case $1 in
    --rm|--network=none|--interactive|--read-only) shift ;;
    --name|--mount|--workdir|--memory|--cpus|--pids-limit|--tmpfs|--security-opt|--cap-drop) [ "$1" = --mount ] && mount=$2; shift 2 ;;
    *) shift; break ;;
  esac
done
root=$(printf '%s' "$mount" | sed -n 's/.*source=\([^,]*\),target=.*/\1/p')
cd "$root" || exit 91
exec "$@"
"#,
            arguments.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o755)).unwrap();

    let server = MockServer::start().await;
    expect_request(&server, "test-key").await;
    let mut config = config(&server.uri(), &secret);
    config.execution_targets.push(ExecutionTargetConfig {
        id: "sandbox:test".into(),
        transport: ExecutionTransportConfig::Sandbox {
            driver: runtime.display().to_string(),
            profile: "test/sandbox:latest".into(),
            resources: crate::config::ContainerResourceSettings::default(),
        },
    });
    config.agents[0].agent_workspace = Some(WorkspaceBindingConfig {
        execution_target: "sandbox:test".into(),
        path: workspace.display().to_string(),
        read_only: true,
        instruction_focus: None,
    });
    let bus: Arc<dyn MessageBus> = Arc::new(InProcessMessageBus::new());
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    let mut agents = build_agents(
        &config,
        bus,
        store.clone(),
        Arc::new(InMemoryMemoryStore::new()),
        None,
        &SystemSecretResolver,
    )
    .unwrap();
    let agent = agents.pop().unwrap();
    let session = persisted_session_with_overrides(
        &agent,
        store.as_ref(),
        "sandbox",
        SessionConfigOverrides {
            user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
                execution_target: "sandbox:test".into(),
                path: workspace,
                read_only: true,
                instruction_focus: None,
            }),
            ..SessionConfigOverrides::default()
        },
    )
    .await;
    agent
        .run
        .handle_message(BusMessage::user_chat(session, "user", "read the workspace"))
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let system = body["system"][0]["text"].as_str().unwrap();
    assert!(system.contains("sandbox-injected-workspace-instruction"));
    let argv = std::fs::read(arguments).unwrap();
    assert!(
        argv.split(|byte| *byte == 0)
            .any(|argument| argument == b"test/sandbox:latest")
    );
}

#[tokio::test]
async fn registry_agent_pins_provider_and_model_but_rotates_credentials_live() {
    let directory = tempdir().unwrap();
    let first_secret = directory.path().join("first.secret");
    let second_secret = directory.path().join("second.secret");
    std::fs::write(&first_secret, "first-key\n").unwrap();
    std::fs::write(&second_secret, "second-key\n").unwrap();
    let pinned_server = MockServer::start().await;
    let newer_server = MockServer::start().await;
    expect_request(&pinned_server, "first-key").await;
    expect_request(&pinned_server, "second-key").await;

    let config = config(&pinned_server.uri(), &first_secret);
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    registry.bootstrap_registries(&config).await.unwrap();
    registry.seed(&config).await.unwrap();
    registry
        .stage_agent_snapshot(AgentSnapshotSelection {
            agent_id: "assistant".into(),
            agent_revision: 1,
            provider_id: "alpha".into(),
            allowed_model_ids: BTreeSet::from(["model-a".into(), "model-b".into()]),
            default_model_id: "model-a".into(),
        })
        .await
        .unwrap();
    let snapshot = registry
        .resolve_registry_composition(&config.agents[0].spec.id, 1)
        .await
        .unwrap();

    let mut newer_provider = snapshot.provider.clone();
    newer_provider.revision = 2;
    newer_provider.base_url = newer_server.uri();
    registry.stage_provider(1, newer_provider).await.unwrap();
    registry.activate_provider("alpha", 2, 1).await.unwrap();
    let mut newer_model = snapshot
        .models
        .iter()
        .find(|model| model.model_id == "model-b")
        .unwrap()
        .clone();
    newer_model.revision = 2;
    registry.stage_model(1, newer_model).await.unwrap();
    registry
        .activate_model(("alpha", "model-b"), 2, 1)
        .await
        .unwrap();

    let bus: Arc<dyn MessageBus> = Arc::new(InProcessMessageBus::new());
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    let sessions: Arc<dyn SessionStore> = store.clone();
    let agent = build_registry_agent(
        &config,
        snapshot.clone(),
        registry.clone(),
        bus,
        sessions,
        Arc::new(InMemoryMemoryStore::new()),
    )
    .unwrap();
    let runtime = agent.run.runtime_model_info().await;
    assert_eq!(runtime.current_model, "model-a");
    assert_eq!(runtime.models.len(), 2);
    assert!(runtime.models.iter().all(|model| model.provider == "alpha"));
    let default =
        resolve_session_config(&agent, &SessionConfigOverrides::default(), None, None).unwrap();
    assert_eq!(
        default.require_revision_pins().unwrap().provider_revision,
        1
    );
    assert_eq!(default.require_revision_pins().unwrap().model_revision, 1);
    let alternate = resolve_session_config(
        &agent,
        &SessionConfigOverrides {
            model: Some(ModelSelection {
                provider_id: "alpha".into(),
                model_id: "model-b".into(),
            }),
            ..SessionConfigOverrides::default()
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        alternate.require_revision_pins().unwrap().provider_revision,
        1
    );
    assert_eq!(alternate.require_revision_pins().unwrap().model_revision, 1);

    let mut missing = agent.clone();
    missing
        .revision_bindings
        .model_revisions
        .remove(&ModelSelection {
            provider_id: "alpha".into(),
            model_id: "model-b".into(),
        });
    assert!(matches!(
        resolve_session_config(
            &missing,
            &SessionConfigOverrides {
                model: Some(ModelSelection {
                    provider_id: "alpha".into(),
                    model_id: "model-b".into(),
                }),
                ..SessionConfigOverrides::default()
            },
            None,
            None,
        ),
        Err(CompositionError::MissingRegistryModelBinding { .. })
    ));

    let first_session = persisted_session(&agent, &store, "first").await;
    agent
        .run
        .handle_message(BusMessage::user_chat(
            first_session,
            "user",
            "first request",
        ))
        .await
        .unwrap();
    registry
        .stage_credential(
            1,
            CredentialBindingRevision {
                binding_id: snapshot.credential_binding_id.clone(),
                generation: 2,
                reference: SecretRef::File {
                    path: second_secret,
                },
            },
        )
        .await
        .unwrap();
    registry
        .activate_credential(&snapshot.credential_binding_id, 2, 1)
        .await
        .unwrap();
    let second_session = persisted_session(&agent, &store, "second").await;
    agent
        .run
        .handle_message(BusMessage::user_chat(
            second_session,
            "user",
            "second request",
        ))
        .await
        .unwrap();

    pinned_server.verify().await;
    assert!(newer_server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn native_v3_routes_exact_providers_without_fallback_and_keeps_live_credentials() {
    let directory = tempdir().unwrap();
    let alpha_first = directory.path().join("alpha-first.secret");
    let alpha_second = directory.path().join("alpha-second.secret");
    let beta_secret = directory.path().join("beta.secret");
    std::fs::write(&alpha_first, "alpha-first-key\n").unwrap();
    std::fs::write(&alpha_second, "alpha-second-key\n").unwrap();
    std::fs::write(&beta_secret, "beta-key\n").unwrap();
    let alpha_pinned = MockServer::start().await;
    let beta_pinned = MockServer::start().await;
    let alpha_new = MockServer::start().await;
    let beta_new = MockServer::start().await;
    let success_stream = TEXT_STREAM.replace("model-a", "shared");
    for key in ["alpha-first-key", "alpha-second-key"] {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("authorization", format!("Bearer {key}")))
            .and(body_partial_json(json!({"model": "shared"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(success_stream.clone(), "text/event-stream"),
            )
            .expect(1)
            .mount(&alpha_pinned)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer beta-key"))
        .and(body_partial_json(json!({"model": "shared"})))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": {"type": "authentication_error", "message": "denied"}
        })))
        .expect(1)
        .mount(&beta_pinned)
        .await;

    let config = dual_provider_config(
        &alpha_pinned.uri(),
        &alpha_first,
        &beta_pinned.uri(),
        &beta_secret,
    );
    let registry = AgentRegistry::open(directory.path().join("dual.db"))
        .await
        .unwrap();
    registry.bootstrap_registries(&config).await.unwrap();
    registry.seed(&config).await.unwrap();
    registry
        .stage_agent_snapshot_v3(AgentSnapshotSelectionV3 {
            agent_id: "assistant".into(),
            agent_revision: 1,
            default_model: ModelSelection {
                provider_id: "alpha".into(),
                model_id: "shared".into(),
            },
            allowed_models: BTreeSet::from([
                ModelSelection {
                    provider_id: "alpha".into(),
                    model_id: "shared".into(),
                },
                ModelSelection {
                    provider_id: "beta".into(),
                    model_id: "shared".into(),
                },
            ]),
        })
        .await
        .unwrap();
    let snapshot = registry
        .resolve_registry_composition_versioned(&config.agents[0].spec.id, 1)
        .await
        .unwrap();

    for (provider_id, newer_url) in [("alpha", alpha_new.uri()), ("beta", beta_new.uri())] {
        let mut provider = snapshot.providers[provider_id].clone();
        provider.revision = 2;
        provider.base_url = newer_url;
        registry.stage_provider(1, provider).await.unwrap();
        registry.activate_provider(provider_id, 2, 1).await.unwrap();
        let selection = ModelSelection {
            provider_id: provider_id.into(),
            model_id: "shared".into(),
        };
        let mut model = snapshot.models[&selection].clone();
        model.revision = 2;
        model.context_window += 1;
        registry.stage_model(1, model).await.unwrap();
        registry
            .activate_model((provider_id, "shared"), 2, 1)
            .await
            .unwrap();
    }

    let bus: Arc<dyn MessageBus> = Arc::new(InProcessMessageBus::new());
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    let sessions: Arc<dyn SessionStore> = store.clone();
    let agent = build_registry_agent_versioned_with_resolver(
        &config,
        snapshot.clone(),
        registry.clone(),
        bus,
        sessions,
        Arc::new(InMemoryMemoryStore::new()),
        None,
        Arc::new(SystemSecretResolver),
        None,
        None,
    )
    .await
    .unwrap();
    let runtime = agent.run.runtime_model_info().await;
    assert_eq!(
        runtime
            .models
            .iter()
            .map(|model| (model.provider.as_str(), model.id.as_str()))
            .collect::<Vec<_>>(),
        vec![("alpha", "shared"), ("beta", "shared")]
    );

    let alpha_one = persisted_session(&agent, &store, "alpha-one").await;
    agent
        .run
        .handle_message(BusMessage::user_chat(alpha_one, "user", "alpha one"))
        .await
        .unwrap();
    let alpha_binding = snapshot.providers["alpha"].credential_binding_id.clone();
    registry
        .stage_credential(
            1,
            CredentialBindingRevision {
                binding_id: alpha_binding.clone(),
                generation: 2,
                reference: SecretRef::File { path: alpha_second },
            },
        )
        .await
        .unwrap();
    registry
        .activate_credential(&alpha_binding, 2, 1)
        .await
        .unwrap();
    let alpha_two = persisted_session(&agent, &store, "alpha-two").await;
    agent
        .run
        .handle_message(BusMessage::user_chat(alpha_two, "user", "alpha two"))
        .await
        .unwrap();

    let beta_selection = ModelSelection {
        provider_id: "beta".into(),
        model_id: "shared".into(),
    };
    agent
        .run
        .select_qualified_model(beta_selection.clone(), ReasoningEffort::Off)
        .await
        .unwrap();
    let beta_session = persisted_session_with_overrides(
        &agent,
        &store,
        "beta",
        SessionConfigOverrides {
            model: Some(beta_selection),
            ..SessionConfigOverrides::default()
        },
    )
    .await;
    let error = agent
        .run
        .handle_message(BusMessage::user_chat(beta_session, "user", "beta"))
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        sylvander_agent::run::AgentRunError::Loop(
            sylvander_agent::error::AgentLoopError::Provider { attempts: 1, source }
        ) if source.kind == sylvander_llm_core::ProviderErrorKind::Authentication
    ));

    alpha_pinned.verify().await;
    beta_pinned.verify().await;
    assert!(alpha_new.received_requests().await.unwrap().is_empty());
    assert!(beta_new.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn public_session_override_survives_restart_and_never_falls_back() {
    const AGENT_PROMPT: &str = "restart agent prompt";
    const PROFILE_PROMPT: &str = "restart beta profile prompt";
    const SESSION_PROMPT: &str = "restart session prompt";
    let directory = tempdir().unwrap();
    let alpha_secret = directory.path().join("alpha.secret");
    let beta_secret = directory.path().join("beta.secret");
    std::fs::write(&alpha_secret, "alpha-key\n").unwrap();
    std::fs::write(&beta_secret, "beta-key\n").unwrap();
    let alpha = MockServer::start().await;
    let beta = MockServer::start().await;
    let success_stream = TEXT_STREAM.replace("model-a", "shared");
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer alpha-key"))
        .and(body_partial_json(json!({"model": "shared"})))
        .respond_with(ResponseTemplate::new(200).set_body_raw(success_stream, "text/event-stream"))
        .expect(1)
        .mount(&alpha)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", "Bearer beta-key"))
        .and(body_partial_json(json!({"model": "shared"})))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": {"type": "authentication_error", "message": "denied"}
        })))
        .expect(1)
        .mount(&beta)
        .await;

    let mut config = dual_provider_config(&alpha.uri(), &alpha_secret, &beta.uri(), &beta_secret);
    config.server.data_dir = Some(directory.path().into());
    config.server.session_db = Some(directory.path().join("runtime.db"));
    crate::configure_test_memory_integrity(&mut config, directory.path(), &alpha_secret);
    config.agents[0].spec.persona.system_prompt = AGENT_PROMPT.into();
    config.agents[0].allow_session_prompt = true;
    config.agents[0].prompt_profiles = vec![crate::config::PromptProfileConfig {
        id: "beta-restart".into(),
        qualified_models: vec![ModelSelection {
            provider_id: "beta".into(),
            model_id: "shared".into(),
        }],
        providers: Vec::new(),
        models: Vec::new(),
        system_prompt: PROFILE_PROMPT.into(),
    }];
    let mut principal = AuthenticatedPrincipal::user("user", AuthenticationMethod::Internal);
    principal.roles.push("admin".into());
    let boundary =
        BoundaryContext::authenticated(principal, "test", "runtime-test", "p1.2-session-model");
    let beta_selection = ModelSelection {
        provider_id: "beta".into(),
        model_id: "shared".into(),
    };

    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    let created = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &boundary,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "beta session".into(),
            channel_id: None,
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    let updated = sylvander_channel::UiService::update_session_config(
        runtime.ui_service.as_ref(),
        &boundary,
        SessionConfigUpdateRequest {
            session_id: created.session_id.clone(),
            expected_revision: created.revision,
            overrides: SessionConfigOverrides {
                model: Some(beta_selection.clone()),
                prompt_profile: Some("beta-restart".into()),
                system_prompt: Some(SESSION_PROMPT.into()),
                ..SessionConfigOverrides::default()
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(updated.effective.model_selection(), beta_selection);
    let expected_effective = updated.effective.clone();
    let expected_prompt_sha256 = expected_effective.system_prompt_sha256.clone();
    let expected_manifest = expected_effective.prompt_manifest.clone();
    runtime.shutdown().await.unwrap();

    let restarted = Runtime::boot_config(config).await.unwrap();
    let restored = sylvander_channel::UiService::session_config(
        restarted.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert_eq!(restored.revision, updated.revision);
    assert_eq!(restored.overrides, updated.overrides);
    assert_eq!(restored.effective.model_selection(), beta_selection);
    assert_eq!(
        restored.effective.system_prompt_sha256,
        expected_prompt_sha256
    );
    assert_eq!(restored.effective, expected_effective);
    assert_eq!(restored.effective.prompt_manifest, expected_manifest);

    let agent = restarted
        .configured_agent(&AgentId::new("assistant"))
        .unwrap();
    let effective_user = restarted
        .session_store
        .get(&created.session_id)
        .await
        .unwrap()
        .unwrap()
        .metadata
        .user_id;
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while agent.run.get_session(&created.session_id).await.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("restored session join must reach the revision worker");
    let beta_error = agent
        .run
        .handle_message(BusMessage::user_chat(
            created.session_id.clone(),
            effective_user.clone(),
            "beta request",
        ))
        .await
        .unwrap_err();
    assert!(
        matches!(
        &beta_error,
        sylvander_agent::run::AgentRunError::Loop(
            sylvander_agent::error::AgentLoopError::Provider { attempts: 1, source }
        ) if source.kind == sylvander_llm_core::ProviderErrorKind::Authentication
        ),
        "unexpected beta failure: {beta_error:?}"
    );
    let beta_requests = beta.received_requests().await.unwrap();
    assert_eq!(beta_requests.len(), 1);
    let beta_body: serde_json::Value =
        serde_json::from_slice(&beta_requests[0].body).expect("provider request must be JSON");
    assert_eq!(beta_body["model"], "shared");
    let actual_prompt = beta_body["system"][0]["text"]
        .as_str()
        .expect("system prompt must be text");
    let expected_layers = [
        sylvander_agent::prompt::SHARED_SAFETY_PROMPT,
        PROFILE_PROMPT,
        AGENT_PROMPT,
        SESSION_PROMPT,
    ];
    let positions = expected_layers.map(|layer| {
        actual_prompt
            .find(layer)
            .expect("the restarted provider call must preserve every persisted prompt layer")
    });
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "the restarted provider call must preserve persisted prompt precedence"
    );

    let alpha_session = sylvander_channel::UiService::create_session(
        restarted.ui_service.as_ref(),
        &boundary,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "alpha session".into(),
            channel_id: None,
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while agent
            .run
            .get_session(&alpha_session.session_id)
            .await
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("new session join must reach the revision worker");
    agent
        .run
        .handle_message(BusMessage::user_chat(
            alpha_session.session_id.clone(),
            effective_user,
            "alpha request",
        ))
        .await
        .unwrap();
    restarted.shutdown().await.unwrap();
    beta.verify().await;
    alpha.verify().await;
}

#[test]
fn registry_revision_binding_validation_is_typed() {
    let provider = ProviderDefinition {
        id: "alpha".into(),
        revision: 1,
        kind: "anthropic_compatible".into(),
        base_url: "https://alpha.invalid".into(),
        credential_binding_id: "provider:alpha:api_key".into(),
    };
    let model = ModelDefinition {
        provider_id: "alpha".into(),
        model_id: "shared".into(),
        revision: 1,
        context_window: 100,
        max_output_tokens: 10,
        capabilities: BTreeSet::new(),
        lifecycle: sylvander_protocol::ModelLifecycle::Active,
        pricing: None,
    };

    let mut zero = provider.clone();
    zero.revision = 0;
    assert!(matches!(
        registry_revision_bindings(&zero, std::slice::from_ref(&model)),
        Err(CompositionError::InvalidRegistryRevisionBinding)
    ));
    assert!(matches!(
        registry_revision_bindings(&provider, &[model.clone(), model.clone()]),
        Err(CompositionError::DuplicateRegistryModelBinding { .. })
    ));
    let mut foreign = model;
    foreign.provider_id = "beta".into();
    assert!(matches!(
        registry_revision_bindings(&provider, &[foreign]),
        Err(CompositionError::RegistryModelProviderMismatch { .. })
    ));
}
