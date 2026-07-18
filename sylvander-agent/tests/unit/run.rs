use super::*;
use crate::bus::InProcessMessageBus;
use crate::tools::memory::InMemoryMemoryStore;
use std::path::PathBuf;

#[allow(clippy::too_many_arguments)]
async fn with_workspace_context(
    prompt: String,
    agent_workspace: Option<&sylvander_protocol::SessionWorkspaceBinding>,
    task_workspace: Option<&sylvander_protocol::SessionWorkspaceBinding>,
    workspace_mounts: &[sylvander_protocol::SessionWorkspaceMount],
    fallback_task_workspace: &Path,
    workspace_executors: &HashMap<String, Arc<dyn WorkspaceExecutor>>,
    skill_features: &std::sync::RwLock<Vec<sylvander_protocol::PlatformFeature>>,
) -> Result<String, AgentRunError> {
    let workspace = workspace_turn_context(
        agent_workspace,
        task_workspace,
        workspace_mounts,
        fallback_task_workspace,
        workspace_executors,
        skill_features,
        "",
        TurnContextBudgets::default().workspace_knowledge,
    )
    .await?;
    Ok(workspace.authoritative.map_or(prompt.clone(), |context| {
        format!("{prompt}\n\n{}", context.content())
    }))
}

impl AgentRun {
    async fn join_session(&self, meta: SessionMetadata) -> SessionId {
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let ctx = SessionContext::new(session_id.clone(), meta);
        self.inner
            .sessions
            .write()
            .await
            .insert(session_id.clone(), ctx);
        self.inner
            .authenticated_sessions
            .write()
            .await
            .insert(session_id.clone());
        session_id
    }

    fn authenticated_session_for_test(&self, session_id: SessionId) -> AuthenticatedSession {
        AuthenticatedSession {
            authority: self.inner.session_authority.clone(),
            session_id,
        }
    }
}

#[test]
fn approval_rejection_reason_is_trimmed_bounded_and_optional() {
    assert_eq!(normalize_rejection_reason(None), "rejected by user");
    assert_eq!(
        normalize_rejection_reason(Some("  \n ")),
        "rejected by user"
    );
    assert_eq!(
        normalize_rejection_reason(Some("  unsafe outside workspace  ")),
        "unsafe outside workspace"
    );
    assert_eq!(
        normalize_rejection_reason(Some(&"x".repeat(501))).len(),
        500
    );
}

async fn next_stream_event(receiver: &mut mpsc::Receiver<BusMessage>) -> StreamEvent {
    loop {
        let message = receiver.recv().await.expect("stream event");
        if let MessageKind::Stream(event) = message.kind {
            return event;
        }
    }
}

fn test_metadata() -> SessionMetadata {
    SessionMetadata {
        workspace: PathBuf::from("/tmp/sylvander-test"),
        name: "test-session".into(),
        user_id: "user-1".into(),
    }
}

fn test_spec_and_client() -> (AgentSpec, AnthropicClient) {
    let spec = AgentSpec::builder()
        .id("test-agent")
        .name("Test")
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec");
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .expect("client");
    (spec, client)
}

#[tokio::test]
async fn turn_prompt_contains_discovered_agent_task_and_skill_context() {
    let agent_home = tempfile::TempDir::new().unwrap();
    let task = tempfile::TempDir::new().unwrap();
    std::fs::write(agent_home.path().join("AGENTS.md"), "agent-home-guide").unwrap();
    std::fs::write(task.path().join("agent.md"), "task-guide").unwrap();
    std::fs::create_dir_all(task.path().join("src/api")).unwrap();
    std::fs::write(task.path().join("src/api/AGENTS.md"), "focused-task-guide").unwrap();
    std::fs::create_dir_all(task.path().join(".agents/skills/test")).unwrap();
    std::fs::write(
        task.path().join(".agents/skills/test/SKILL.md"),
        "skill-guide",
    )
    .unwrap();

    let executors = HashMap::from([(
        "local".to_owned(),
        Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>,
    )]);
    let skill_features = std::sync::RwLock::new(Vec::new());
    let mounts = vec![sylvander_protocol::SessionWorkspaceMount {
        reference: "docs".into(),
        role: sylvander_protocol::WorkspaceMountRole::Dependency,
        binding: sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: task.path().into(),
            read_only: true,
            instruction_focus: None,
        },
        capabilities: sylvander_protocol::WorkspaceCapabilityPolicy {
            read: true,
            git: true,
            ..Default::default()
        },
    }];
    let prompt = with_workspace_context(
        "base-prompt".into(),
        Some(&sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: agent_home.path().to_path_buf(),
            read_only: true,
            instruction_focus: None,
        }),
        Some(&sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: task.path().to_path_buf(),
            read_only: false,
            instruction_focus: Some("src/api".into()),
        }),
        &mounts,
        task.path(),
        &executors,
        &skill_features,
    )
    .await
    .unwrap();
    let base = prompt.find("base-prompt").unwrap();
    let agent = prompt.find("agent-home-guide").unwrap();
    let skills = skill_features.read().unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "test");
    assert_eq!(
        skills[0].trust,
        Some(sylvander_protocol::PlatformTrust::Workspace)
    );
    assert_eq!(
        skills[0].status,
        sylvander_protocol::PlatformFeatureStatus::Active
    );
    assert!(
        skills[0]
            .capabilities
            .contains(&"prompt_instructions".to_owned())
    );
    assert!(skills[0].reloadable);
    let task = prompt.find("task-guide").unwrap();
    let skill = prompt.find("skill-guide").unwrap();
    let focused = prompt.find("focused-task-guide").unwrap();
    assert!(base < agent && agent < task && task < focused && focused < skill);
    assert!(prompt.contains("@docs (dependency): read, git"));
    assert!(prompt.contains("`@reference/path`"));
}

#[derive(Default)]
struct RecordingProvider {
    requests: std::sync::Mutex<Vec<sylvander_llm_core::ModelRequest>>,
}

#[derive(Clone)]
struct FixedUserProfile(sylvander_protocol::UserProfileView);

fn profile_with_learning(do_not_learn: bool) -> sylvander_protocol::UserProfileView {
    sylvander_protocol::UserProfileView {
        revision: 1,
        profile: sylvander_protocol::UserProfileData::default(),
        do_not_learn,
        created_at_unix_secs: 1,
        updated_at_unix_secs: 1,
    }
}

#[async_trait::async_trait]
impl crate::user_profile_provider::UserProfileProvider for FixedUserProfile {
    async fn current_profile(
        &self,
        _subject: &crate::user_profile_provider::UserProfileSubject,
    ) -> Result<
        Option<sylvander_protocol::UserProfileView>,
        crate::user_profile_provider::UserProfileProviderError,
    > {
        Ok(Some(self.0.clone()))
    }
}

struct UnavailableUserProfile;

#[async_trait::async_trait]
impl crate::user_profile_provider::UserProfileProvider for UnavailableUserProfile {
    async fn current_profile(
        &self,
        _subject: &crate::user_profile_provider::UserProfileSubject,
    ) -> Result<
        Option<sylvander_protocol::UserProfileView>,
        crate::user_profile_provider::UserProfileProviderError,
    > {
        Err(crate::user_profile_provider::UserProfileProviderError::Unavailable)
    }
}

#[derive(Debug)]
struct MarkerWorkspaceExecutor {
    marker: &'static [u8],
    reads: std::sync::Mutex<Vec<WorkspaceTarget>>,
}

impl MarkerWorkspaceExecutor {
    fn new(marker: &'static [u8]) -> Self {
        Self {
            marker,
            reads: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl WorkspaceExecutor for MarkerWorkspaceExecutor {
    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        _relative_path: &str,
    ) -> Result<Vec<u8>, crate::workspace_executor::WorkspaceExecutorError> {
        self.reads.lock().unwrap().push(target.clone());
        Ok(self.marker.to_vec())
    }

    async fn write_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
        _content: &[u8],
    ) -> Result<(), crate::workspace_executor::WorkspaceExecutorError> {
        Ok(())
    }

    async fn run_command(
        &self,
        _target: &WorkspaceTarget,
        _command: &str,
        _timeout: std::time::Duration,
    ) -> Result<
        crate::workspace_executor::WorkspaceCommandOutput,
        crate::workspace_executor::WorkspaceExecutorError,
    > {
        Ok(crate::workspace_executor::WorkspaceCommandOutput {
            success: true,
            status_code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
        })
    }

    async fn list(
        &self,
        _target: &WorkspaceTarget,
        request: crate::workspace_executor::WorkspaceListRequest,
    ) -> Result<
        crate::workspace_executor::WorkspaceListResult,
        crate::workspace_executor::WorkspaceExecutorError,
    > {
        let entries = (request.relative_path == ".")
            .then(|| crate::workspace_executor::WorkspaceListEntry {
                relative_path: "AGENTS.md".into(),
                kind: crate::workspace_executor::WorkspaceEntryKind::File,
                size: self.marker.len() as u64,
            })
            .into_iter()
            .collect();
        Ok(crate::workspace_executor::WorkspaceListResult {
            entries,
            truncated: false,
        })
    }
}

#[tokio::test]
async fn workspace_prompt_uses_each_execution_target_without_local_filesystem_access() {
    let agent = Arc::new(MarkerWorkspaceExecutor::new(b"remote-agent-guide"));
    let task = Arc::new(MarkerWorkspaceExecutor::new(b"remote-task-guide"));
    let executors = HashMap::from([
        (
            "ssh:agent".to_owned(),
            agent.clone() as Arc<dyn WorkspaceExecutor>,
        ),
        (
            "ssh:task".to_owned(),
            task.clone() as Arc<dyn WorkspaceExecutor>,
        ),
    ]);
    let prompt = with_workspace_context(
        "base".into(),
        Some(&sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "ssh:agent".into(),
            path: "/remote/agent".into(),
            read_only: true,
            instruction_focus: None,
        }),
        Some(&sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "ssh:task".into(),
            path: "/remote/task".into(),
            read_only: false,
            instruction_focus: None,
        }),
        &[],
        Path::new("/attached/task"),
        &executors,
        &std::sync::RwLock::new(Vec::new()),
    )
    .await
    .unwrap();

    assert!(prompt.contains("remote-agent-guide"));
    assert!(prompt.contains("remote-task-guide"));
    assert_eq!(
        agent.reads.lock().unwrap()[0].workspace_path,
        Path::new("/remote/agent")
    );
    assert_eq!(
        task.reads.lock().unwrap()[0].workspace_path,
        Path::new("/remote/task")
    );
}

fn remote_effective_config(
    target_id: &str,
    workspace: &str,
) -> sylvander_protocol::SessionEffectiveConfig {
    let source = || sylvander_protocol::SessionConfigSource {
        kind: sylvander_protocol::SessionConfigSourceKind::RequestOverride,
        reference: None,
    };
    sylvander_protocol::SessionEffectiveConfig {
        agent_id: AgentId::new("test-agent"),
        agent_revision: 1,
        provider_id: "test".into(),
        provider_revision: 1,
        model_id: "test".into(),
        model_revision: 1,
        reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
        permissions: sylvander_protocol::PermissionProfile::default(),
        prompt_profile: None,
        system_prompt_sha256: String::new(),
        prompt_manifest: sylvander_protocol::PromptManifest {
            layers: Vec::new(),
            aggregate_sha256: String::new(),
            total_bytes: 0,
        },
        agent_workspace: None,
        user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
            execution_target: target_id.into(),
            path: workspace.into(),
            read_only: false,
            instruction_focus: None,
        }),
        workspace_mounts: Vec::new(),
        execution_target: target_id.into(),
        provenance: sylvander_protocol::SessionConfigProvenance {
            model: source(),
            reasoning_effort: source(),
            permissions: source(),
            prompt_profile: source(),
            system_prompt: source(),
            agent_workspace: source(),
            user_workspace: source(),
            execution_target: source(),
        },
    }
}

impl sylvander_llm_core::ModelProvider for RecordingProvider {
    fn complete_stream(
        &self,
        request: sylvander_llm_core::ModelRequest,
    ) -> sylvander_llm_core::ProviderFuture<'_> {
        self.requests.lock().unwrap().push(request.clone());
        Box::pin(async move {
            let response = sylvander_llm_core::ModelResponse {
                id: request.request_id,
                model: request.model,
                content: vec![sylvander_llm_core::ContentBlock::Text { text: "ok".into() }],
                stop_reason: sylvander_llm_core::StopReason::EndTurn,
                usage: sylvander_llm_core::TokenUsage::default(),
            };
            Ok(Box::pin(futures_util::stream::iter([Ok(
                sylvander_llm_core::ModelStreamEvent::Completed(response),
            )])) as sylvander_llm_core::ModelEventStream)
        })
    }
}

#[tokio::test]
async fn durable_turn_prompt_uses_attached_workspace_instead_of_stale_binding() {
    let source = tempfile::TempDir::new().unwrap();
    let worktree = tempfile::TempDir::new().unwrap();
    std::fs::write(source.path().join("AGENTS.md"), "source-workspace-guide").unwrap();
    std::fs::write(
        worktree.path().join("AGENTS.md"),
        "effective-worktree-guide",
    )
    .unwrap();

    let store: Arc<dyn SessionStore> = Arc::new(
        crate::session_store::SqliteSessionStore::open_in_memory()
            .await
            .unwrap(),
    );
    let (spec, _) = test_spec_and_client();
    let resolver = Arc::new(
        crate::prompt::PromptResolver::new(
            "agent:test-agent@1".into(),
            spec.persona.system_prompt.clone(),
            Vec::new(),
            None,
            false,
        )
        .unwrap(),
    );
    let provider = Arc::new(RecordingProvider::default());
    let model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new(
            spec.model.provider.clone(),
            spec.model.model_name.clone(),
        ),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let run = AgentRun::provider_builder(spec, provider.clone(), model)
        .bus(Arc::new(InProcessMessageBus::new()))
        .session_store(store.clone())
        .prompt_resolver(resolver)
        .build()
        .unwrap();
    let metadata = SessionMetadata {
        workspace: worktree.path().to_path_buf(),
        ..test_metadata()
    };
    let session_id = run.join_session(metadata.clone()).await;
    let mut stored = StoredSession::new(
        session_id.clone(),
        metadata.name.clone(),
        SessionLifetime::Persistent,
        metadata.clone(),
        vec![run.id().clone()],
    );
    stored.effective_config = Some(run.inner.direct_session_config(&metadata).await);
    stored
        .effective_config
        .as_mut()
        .unwrap()
        .user_workspace
        .as_mut()
        .unwrap()
        .path = source.path().to_path_buf();
    store.save(&stored).await.unwrap();

    run.handle_message(BusMessage::user_chat(
        session_id,
        metadata.user_id,
        "inspect the workspace",
    ))
    .await
    .unwrap();

    let system = {
        let requests = provider.requests.lock().unwrap();
        requests[0]
            .system
            .iter()
            .map(|instruction| instruction.text.as_str())
            .collect::<String>()
    };
    assert!(system.contains("effective-worktree-guide"));
    assert!(!system.contains("source-workspace-guide"));
}

#[tokio::test]
async fn live_turn_injects_all_typed_context_layers_and_exposes_a_manifest() {
    let workspace = tempfile::TempDir::new().unwrap();
    std::fs::write(
        workspace.path().join("AGENTS.md"),
        "workspace instructions stay below runtime safety",
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("knowledge.md"),
        "typed context retrieval must stay bounded and relevant\n",
    )
    .unwrap();

    let memory = Arc::new(InMemoryMemoryStore::new());
    let memory_caller =
        sylvander_protocol::SessionContext::new("user-1", "test-agent", "memory-seed");
    let memory_context = MemoryExecutionContext::application_worker(&memory_caller);
    memory
        .append_relationship(
            &memory_context,
            MemoryAppend::new("typed context should prefer relevant relationship memory"),
        )
        .await
        .unwrap();
    memory
        .append_relationship(
            &memory_context,
            MemoryAppend::new("unrelated favorite lunch"),
        )
        .await
        .unwrap();

    let store: Arc<dyn SessionStore> = Arc::new(
        crate::session_store::SqliteSessionStore::open_in_memory()
            .await
            .unwrap(),
    );
    let (spec, _) = test_spec_and_client();
    let selection = sylvander_protocol::ModelSelection {
        provider_id: spec.model.provider.clone(),
        model_id: spec.model.model_name.clone(),
    };
    let resolver = Arc::new(
        crate::prompt::PromptResolver::new(
            "agent:test-agent@3".into(),
            "agent persona".into(),
            Vec::new(),
            None,
            true,
        )
        .unwrap(),
    );
    let profile = sylvander_protocol::UserProfileView {
        revision: 9,
        profile: sylvander_protocol::UserProfileData {
            preferred_language: Some(sylvander_protocol::ClassifiedPreference {
                value: sylvander_protocol::LanguageTag::new("zh-CN").unwrap(),
                privacy_class: sylvander_protocol::PrivacyClass::Personal,
            }),
            ..sylvander_protocol::UserProfileData::default()
        },
        do_not_learn: false,
        created_at_unix_secs: 1,
        updated_at_unix_secs: 2,
    };
    let provider = Arc::new(RecordingProvider::default());
    let model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new(
            selection.provider_id.clone(),
            selection.model_id.clone(),
        ),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let run = AgentRun::provider_builder(spec, provider.clone(), model)
        .bus(Arc::new(InProcessMessageBus::new()))
        .session_store(store.clone())
        .memory(memory)
        .prompt_resolver(resolver.clone())
        .user_profile_provider(Arc::new(FixedUserProfile(profile)))
        .build()
        .unwrap();
    let metadata = SessionMetadata {
        workspace: workspace.path().to_path_buf(),
        ..test_metadata()
    };
    let session_id = run.join_session(metadata.clone()).await;
    let authenticated = run.authenticated_session_for_test(session_id.clone());
    let mut stored = StoredSession::new(
        session_id.clone(),
        metadata.name.clone(),
        SessionLifetime::Persistent,
        metadata.clone(),
        vec![run.id().clone()],
    );
    stored.config_overrides.system_prompt = Some("respond with evidence".into());
    let prompt_snapshot = resolver
        .resolve(&selection, None, Some("respond with evidence"))
        .unwrap();
    let mut effective = run.inner.direct_session_config(&metadata).await;
    effective.agent_revision = 3;
    effective.system_prompt_sha256 = prompt_snapshot.system_prompt_sha256;
    effective.prompt_manifest = prompt_snapshot.manifest;
    stored.effective_config = Some(effective);
    store.save(&stored).await.unwrap();

    run.handle_message(BusMessage::user_chat(
        session_id,
        metadata.user_id,
        "explain typed context retrieval",
    ))
    .await
    .unwrap();

    let system = {
        let requests = provider.requests.lock().unwrap();
        requests[0]
            .system
            .iter()
            .map(|instruction| instruction.text.as_str())
            .collect::<String>()
    };
    let positions = [
        "kind=safety",
        "kind=agent",
        "kind=user_profile",
        "kind=relationship_memory",
        "kind=workspace_knowledge",
        "kind=session",
    ]
    .map(|marker| system.find(marker).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    assert!(system.contains("relevant relationship memory"));
    assert!(system.contains("knowledge.md:1"));
    assert!(system.contains("respond with evidence"));
    assert!(!system.contains("favorite lunch"));
    let manifest = run
        .turn_context_manifest(&authenticated)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        manifest.schema_version,
        crate::turn_context::TURN_CONTEXT_SCHEMA_VERSION
    );
    assert_eq!(manifest.layers.len(), 6);
    assert_eq!(manifest.aggregate_sha256.len(), 64);
    assert!(
        manifest
            .layers
            .iter()
            .all(|layer| !layer.included_items.is_empty())
    );
}

#[tokio::test]
async fn identity_and_prompt_integrity_fail_before_provider_and_durable_turn_writes() {
    #[derive(Clone, Copy)]
    enum Tamper {
        SenderIdentity,
        SystemHash,
        LayerHash,
    }

    for tamper in [
        Tamper::SenderIdentity,
        Tamper::SystemHash,
        Tamper::LayerHash,
    ] {
        let directory = tempfile::TempDir::new().expect("temporary directory");
        let database = directory.path().join("sessions.db");
        let store: Arc<dyn SessionStore> = Arc::new(
            crate::session_store::SqliteSessionStore::open(&database)
                .await
                .expect("store"),
        );
        let (spec, _) = test_spec_and_client();
        let selection = sylvander_protocol::ModelSelection {
            provider_id: spec.model.provider.clone(),
            model_id: spec.model.model_name.clone(),
        };
        let resolver = Arc::new(
            crate::prompt::PromptResolver::new(
                "agent:test-agent@1".into(),
                spec.persona.system_prompt.clone(),
                Vec::new(),
                None,
                true,
            )
            .expect("prompt resolver"),
        );
        let prompt_snapshot = resolver
            .resolve(&selection, None, Some("private prompt sentinel"))
            .expect("resolved prompt");
        let provider = Arc::new(RecordingProvider::default());
        let model = ProviderModelInfo {
            reference: sylvander_llm_core::ModelRef::new(
                selection.provider_id.clone(),
                selection.model_id.clone(),
            ),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: sylvander_llm_core::ModelCapabilities::empty(),
        };
        let run = AgentRun::provider_builder(spec, provider.clone(), model)
            .bus(Arc::new(InProcessMessageBus::new()))
            .session_store(store.clone())
            .prompt_resolver(resolver)
            .build()
            .expect("run");
        let metadata = test_metadata();
        let session_id = run.join_session(metadata.clone()).await;
        let mut stored = StoredSession::new(
            session_id.clone(),
            metadata.name.clone(),
            SessionLifetime::Persistent,
            metadata.clone(),
            vec![run.id().clone()],
        );
        stored.config_overrides.system_prompt = Some("private prompt sentinel".into());
        let mut effective = run.inner.direct_session_config(&metadata).await;
        effective.agent_revision = 1;
        effective.system_prompt_sha256 = prompt_snapshot.system_prompt_sha256;
        effective.prompt_manifest = prompt_snapshot.manifest;
        match tamper {
            Tamper::SenderIdentity => {}
            Tamper::SystemHash => effective.system_prompt_sha256 = "tampered".into(),
            Tamper::LayerHash => {
                effective.prompt_manifest.layers[0].sha256 = "tampered".into();
            }
        }
        stored.effective_config = Some(effective);
        store.save(&stored).await.expect("save tampered session");

        let error = run
            .handle_message(BusMessage::user_chat(
                session_id.clone(),
                if matches!(tamper, Tamper::SenderIdentity) {
                    "different-user"
                } else {
                    "user-1"
                },
                "must not execute",
            ))
            .await
            .expect_err("invalid session inputs must fail closed");
        let rendered = error.to_string();
        assert_eq!(
            rendered,
            if matches!(tamper, Tamper::SenderIdentity) {
                "session configuration error: session identity verification failed"
            } else {
                "session configuration error: prompt integrity verification failed"
            }
        );
        assert!(!rendered.contains("private prompt sentinel"));
        assert!(provider.requests.lock().unwrap().is_empty());

        let connection = rusqlite::Connection::open(&database).expect("inspect database");
        for table in ["session_turn_configs", "session_messages"] {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("row count");
            assert_eq!(count, 0, "{table} must remain untouched");
        }
    }
}

#[tokio::test]
async fn provider_catalog_is_qualified_and_turn_snapshot_uses_exact_model() {
    let mut spec = AgentSpec::builder()
        .id("provider-agent")
        .name("Provider")
        .model_name("shared")
        .build()
        .unwrap();
    spec.model.provider = "local".into();
    let provider = Arc::new(RecordingProvider::default());
    let provider_model = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("local", "shared"),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let alternate = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("local", "model-b"),
        context_window: 200_000,
        max_output_tokens: 8192,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let foreign = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("remote", "shared"),
        context_window: 300_000,
        max_output_tokens: 16_384,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let run = AgentRun::provider_builder(spec, provider.clone(), provider_model)
        .bus(Arc::new(InProcessMessageBus::new()))
        .available_provider_models(vec![alternate, foreign])
        .build()
        .unwrap();

    let before = run.runtime_model_info().await;
    assert_eq!(before.models.len(), 3);
    assert!(
        run.select_model("shared", sylvander_protocol::ReasoningEffort::Off)
            .await
            .is_err()
    );
    assert!(
        run.select_qualified_model(
            sylvander_protocol::ModelSelection {
                provider_id: "remote".into(),
                model_id: "shared".into(),
            },
            sylvander_protocol::ReasoningEffort::Off,
        )
        .await
        .is_err()
    );
    assert_eq!(
        run.runtime_model_info().await.current_model,
        before.current_model
    );
    run.select_model("model-b", sylvander_protocol::ReasoningEffort::Off)
        .await
        .unwrap();
    let selected = {
        let runtime = run.inner.runtime_models.read().await;
        runtime.available.get(&runtime.current).unwrap().clone()
    };
    let snapshot = run
        .inner
        .prepare_loop_snapshot(&selected, sylvander_protocol::ReasoningEffort::Off)
        .unwrap();

    crate::loop_::run(
        &snapshot,
        vec![sylvander_llm_anthropic::api::types::MessageParam::user(
            "hello",
        )],
    )
    .await
    .unwrap();
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].model,
        sylvander_llm_core::ModelRef::new("local", "model-b")
    );
}

#[tokio::test]
async fn qualified_router_crosses_providers_without_metadata_collisions() {
    let mut spec = AgentSpec::builder()
        .id("router-agent")
        .name("Router")
        .model_name("shared")
        .build()
        .unwrap();
    spec.model.provider = "local".into();
    let router = Arc::new(RecordingProvider::default());
    let local = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("local", "shared"),
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: sylvander_llm_core::ModelCapabilities::empty(),
    };
    let remote = ProviderModelInfo {
        reference: sylvander_llm_core::ModelRef::new("remote", "shared"),
        context_window: 200_000,
        max_output_tokens: 8192,
        capabilities: sylvander_llm_core::ModelCapabilities::TOOL_USE
            | sylvander_llm_core::ModelCapabilities::VISION,
    };
    let local_selection = sylvander_protocol::ModelSelection {
        provider_id: "local".into(),
        model_id: "shared".into(),
    };
    let remote_selection = sylvander_protocol::ModelSelection {
        provider_id: "remote".into(),
        model_id: "shared".into(),
    };
    let remote_pricing = sylvander_protocol::ModelPricing {
        input_usd_micros_per_million: 11,
        output_usd_micros_per_million: 22,
        cache_write_usd_micros_per_million: None,
        cache_read_usd_micros_per_million: None,
    };
    let run = AgentRun::qualified_router_builder(spec, router.clone(), local)
        .bus(Arc::new(InProcessMessageBus::new()))
        .available_provider_models(vec![remote])
        .qualified_model_lifecycles(HashMap::from([
            (local_selection, sylvander_protocol::ModelLifecycle::Active),
            (
                remote_selection.clone(),
                sylvander_protocol::ModelLifecycle::Deprecated { replacement: None },
            ),
        ]))
        .qualified_model_pricing(HashMap::from([(remote_selection.clone(), remote_pricing)]))
        .build()
        .unwrap();

    let advertised = run.runtime_model_info().await;
    let local = advertised
        .models
        .iter()
        .find(|model| model.provider == "local" && model.id == "shared")
        .unwrap();
    let remote = advertised
        .models
        .iter()
        .find(|model| model.provider == "remote" && model.id == "shared")
        .unwrap();
    assert_eq!(local.lifecycle, sylvander_protocol::ModelLifecycle::Active);
    assert_eq!(local.pricing, None);
    assert!(matches!(
        remote.lifecycle,
        sylvander_protocol::ModelLifecycle::Deprecated { .. }
    ));
    assert_eq!(remote.pricing, Some(remote_pricing));
    assert_eq!(
        remote.capability_names,
        [
            sylvander_protocol::ModelCapability::ToolUse,
            sylvander_protocol::ModelCapability::Vision,
        ]
    );

    run.select_qualified_model(remote_selection, sylvander_protocol::ReasoningEffort::Off)
        .await
        .unwrap();
    let selected = {
        let runtime = run.inner.runtime_models.read().await;
        runtime.available.get(&runtime.current).unwrap().clone()
    };
    let snapshot = run
        .inner
        .prepare_loop_snapshot(&selected, sylvander_protocol::ReasoningEffort::Off)
        .unwrap();
    crate::loop_::run(
        &snapshot,
        vec![sylvander_llm_anthropic::api::types::MessageParam::user(
            "hello",
        )],
    )
    .await
    .unwrap();
    assert_eq!(
        router.requests.lock().unwrap()[0].model,
        sylvander_llm_core::ModelRef::new("remote", "shared")
    );
}

#[tokio::test]
async fn provider_manual_compaction_uses_backend_factory() {
    let mut spec = AgentSpec::builder()
        .id("provider-agent")
        .name("Provider")
        .model_name("model-a")
        .build()
        .unwrap();
    spec.model.provider = "local".into();
    let provider = Arc::new(RecordingProvider::default());
    let run = AgentRun::provider_builder(
        spec,
        provider.clone(),
        ProviderModelInfo {
            reference: sylvander_llm_core::ModelRef::new("local", "model-a"),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: sylvander_llm_core::ModelCapabilities::empty(),
        },
    )
    .bus(Arc::new(InProcessMessageBus::new()))
    .build()
    .unwrap();
    let session_id = run.join_session(test_metadata()).await;
    {
        let mut sessions = run.inner.sessions.write().await;
        let session = sessions.get_mut(&session_id).unwrap();
        for index in 0..6 {
            session.append_user_message(sylvander_llm_anthropic::api::types::MessageParam::user(
                format!("message {index}"),
            ));
        }
    }

    let report = run.compact_session(&session_id).await.unwrap();
    assert_eq!(report.removed_messages, 2);
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    assert_eq!(run.get_session(&session_id).await.unwrap().len(), 5);
}

#[tokio::test]
async fn manual_compaction_failures_are_typed_before_string_facade() {
    use crate::compress::error::CompactionFailureCode;

    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build()
        .unwrap();
    let missing = SessionId::new("missing");
    assert_eq!(
        run.compact_session_typed(&missing).await.unwrap_err().code,
        CompactionFailureCode::SessionUnavailable
    );
    let session_id = run.join_session(test_metadata()).await;
    assert_eq!(
        run.compact_session_typed(&session_id)
            .await
            .unwrap_err()
            .code,
        CompactionFailureCode::InsufficientHistory
    );
    let (interrupt, _receiver) = oneshot::channel();
    run.inner.active_turns.lock().await.insert(
        session_id.clone(),
        ActiveTurn {
            id: uuid::Uuid::new_v4(),
            interrupt,
        },
    );
    assert_eq!(
        run.compact_session_typed(&session_id)
            .await
            .unwrap_err()
            .code,
        CompactionFailureCode::Busy
    );
}

#[test]
fn turn_correlation_keeps_request_and_trace_boundaries_explicit() {
    let message = BusMessage::user_chat(SessionId::new("session"), "user", "hello");
    let request_id = message.id.0.to_string();
    let turn_id = uuid::Uuid::parse_str("13fcf8b4-31f8-4b3a-9432-0cc9ad73d7c0").unwrap();

    let correlation = TurnCorrelation::new(&message, turn_id);

    assert_eq!(correlation.request, request_id);
    assert_eq!(correlation.turn, turn_id.to_string());
    assert_eq!(correlation.trace, correlation.turn);
}

#[test]
fn platform_snapshot_is_truthful_and_redacts_configuration_secrets() {
    let spec = AgentSpec::builder()
        .id("test-agent")
        .name("Test")
        .model_name("test-model")
        .mcp_server_def(crate::spec::McpServerConfig {
            name: "search".into(),
            command: "/opt/bin/search-mcp".into(),
            args: vec!["--token".into(), "also-secret".into()],
            envs: std::collections::HashMap::from([("SEARCH_TOKEN".into(), "super-secret".into())]),
        })
        .ui_command(crate::spec::UiCommandConfig {
            id: "security-review".into(),
            name: "security-review".into(),
            usage: "/security-review [scope]".into(),
            description: "Review a scope".into(),
            hint: "workspace".into(),
            prompt: "Review {{args}} for security issues.".into(),
        })
        .tool_presentations(vec![crate::spec::ToolPresentationConfig {
            tool_name: "search".into(),
            label: "Search".into(),
            kind: sylvander_protocol::ToolPresentationKind::Search,
            target_field: Some("query".into()),
        }])
        .build()
        .unwrap();
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .unwrap();
    let run = AgentRun::builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .memory(Arc::new(InMemoryMemoryStore::new()))
        .build()
        .unwrap();

    let snapshot = run.platform_snapshot();
    assert_eq!(snapshot.features.len(), 3);
    assert_eq!(snapshot.commands.len(), 1);
    assert_eq!(snapshot.tool_presentations.len(), 1);
    assert_eq!(snapshot.commands[0].source, "agent configuration");
    assert_eq!(
        snapshot.commands[0].trust,
        sylvander_protocol::PlatformTrust::Workspace
    );
    assert_eq!(
        snapshot.features[0].status,
        sylvander_protocol::PlatformFeatureStatus::Configured
    );
    assert_eq!(
        snapshot.features[1].kind,
        sylvander_protocol::PlatformFeatureKind::Memory
    );
    assert_eq!(snapshot.features[1].name, "runtime memory");
    assert_eq!(
        snapshot.features[1].status,
        sylvander_protocol::PlatformFeatureStatus::Active
    );
    assert_eq!(
        snapshot.features[1].source.as_deref(),
        Some("runtime injection")
    );
    assert_eq!(
        snapshot.features[2].kind,
        sylvander_protocol::PlatformFeatureKind::Extension
    );
    let json = serde_json::to_string(&snapshot).unwrap();
    assert!(!json.contains("super-secret"));
    assert!(!json.contains("also-secret"));
    assert!(!json.contains("/opt/bin"));
}

#[test]
fn platform_snapshot_reports_runtime_override_without_activating_declarations() {
    let spec = AgentSpec::builder()
        .id("test-agent")
        .name("Test")
        .model_name("test-model")
        .memory_store(crate::spec::MemoryStoreConfig {
            store_type: "sqlite".into(),
            path: PathBuf::from("/private/sentinel-memory.db"),
        })
        .build()
        .unwrap();
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .unwrap();
    let run = AgentRun::builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .memory(Arc::new(InMemoryMemoryStore::new()))
        .build()
        .unwrap();

    let snapshot = run.platform_snapshot();
    let memory = snapshot
        .features
        .iter()
        .filter(|feature| feature.kind == sylvander_protocol::PlatformFeatureKind::Memory)
        .collect::<Vec<_>>();
    assert_eq!(memory.len(), 2);
    assert_eq!(
        memory
            .iter()
            .filter(|feature| {
                feature.status == sylvander_protocol::PlatformFeatureStatus::Active
            })
            .count(),
        1
    );
    assert_eq!(memory[0].name, "runtime memory");
    assert_eq!(memory[1].name, "sqlite");
    assert_eq!(
        memory[1].status,
        sylvander_protocol::PlatformFeatureStatus::Configured
    );
    assert_eq!(memory[1].source.as_deref(), Some("agent configuration"));
    assert!(memory[1].capabilities.is_empty());
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("sentinel-memory")
    );
}

#[test]
fn agent_memory_declarations_are_not_implicit_runtime_fallbacks() {
    let spec = AgentSpec::builder()
        .id("test-agent")
        .name("Test")
        .model_name("test-model")
        .memory_store(crate::spec::MemoryStoreConfig {
            store_type: "unsupported-future-store".into(),
            path: PathBuf::from("/private/never-open-this-store"),
        })
        .build()
        .unwrap();
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .unwrap();
    let run = AgentRun::builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build()
        .unwrap();

    assert!(run.inner.memory.is_none());
    let snapshot = run.platform_snapshot();
    let memory = snapshot
        .features
        .iter()
        .filter(|feature| feature.kind == sylvander_protocol::PlatformFeatureKind::Memory)
        .collect::<Vec<_>>();
    assert_eq!(memory.len(), 1);
    assert_eq!(
        memory[0].status,
        sylvander_protocol::PlatformFeatureStatus::Configured
    );
    assert_eq!(memory[0].summary, "declared; not activated by runtime");
    assert!(memory[0].capabilities.is_empty());
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("never-open-this-store")
    );
}

#[tokio::test(start_paused = true)]
async fn approval_timeout_rejects_and_clears_the_pending_request() {
    let bus = Arc::new(InProcessMessageBus::new());
    let mut events = bus.subscribe(SubscriptionFilter::all()).await.unwrap();
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let gate = Arc::new(BusApprovalGate {
        bus,
        agent_id: AgentId::new("agent"),
        session_id: SessionId::new("session"),
        grant_context: ApprovalGrantContext::new(
            "user",
            AgentId::new("agent"),
            format!("sha256:{}", "1".repeat(64)),
            format!("sha256:{}", "2".repeat(64)),
        ),
        persistent_identity_authorized: true,
        pending_approvals: pending.clone(),
        approval_memory: Arc::new(Mutex::new(ApprovalMemory::load(None).unwrap())),
    });
    let request = ToolUseRequest {
        call_id: "tool-1".into(),
        tool_name: "write".into(),
        input: serde_json::json!({"path": "notes.md"}),
    };
    let task = tokio::spawn(async move { gate.check_batch(&[request]).await });

    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::ToolApprovalRequired { .. }
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(121)).await;
    let result = task.await.unwrap();

    assert!(matches!(
        result.decisions.as_slice(),
        [ApprovalDecision::Rejected { reason }] if reason == "approval timeout"
    ));
    assert!(pending.lock().await.is_empty());
    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::InteractionTimedOut {
            kind: sylvander_protocol::InteractionTimeoutKind::Approval,
            subject_id,
            timeout_secs: 120,
            recovery: sylvander_protocol::TimeoutRecovery::RetryRequest,
        } if subject_id == "tool-1"
    ));
}

#[tokio::test(start_paused = true)]
async fn question_timeout_returns_empty_and_clears_the_pending_answer() {
    let bus = Arc::new(InProcessMessageBus::new());
    let mut events = bus.subscribe(SubscriptionFilter::all()).await.unwrap();
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let gate = Arc::new(BusAskUserGate {
        bus,
        agent_id: AgentId::new("agent"),
        session_id: SessionId::new("session"),
        pending_answers: pending.clone(),
    });
    let task =
        tokio::spawn(async move { gate.ask("question-1", "Continue?", vec![], false).await });

    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::AskUser { .. }
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(301)).await;

    assert!(task.await.unwrap().is_empty());
    assert!(pending.lock().await.is_empty());
    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::InteractionTimedOut {
            kind: sylvander_protocol::InteractionTimeoutKind::Question,
            subject_id,
            timeout_secs: 300,
            recovery: sylvander_protocol::TimeoutRecovery::RetryRequest,
        } if subject_id == "question-1"
    ));
}

#[tokio::test(start_paused = true)]
async fn plan_timeout_rejects_and_clears_the_pending_review() {
    let bus = Arc::new(InProcessMessageBus::new());
    let mut events = bus.subscribe(SubscriptionFilter::all()).await.unwrap();
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let gate = Arc::new(BusPlanGate {
        bus,
        agent_id: AgentId::new("agent"),
        session_id: SessionId::new("session"),
        pending_plans: pending.clone(),
    });
    let task = tokio::spawn(async move { gate.review("plan-1", vec!["inspect".into()]).await });

    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::PlanProposed { .. }
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(301)).await;

    assert!(matches!(
        task.await.unwrap(),
        crate::bus::PlanDecision::Rejected { reason } if reason == "plan review timed out"
    ));
    assert!(pending.lock().await.is_empty());
    assert!(matches!(
        next_stream_event(&mut events).await,
        StreamEvent::InteractionTimedOut {
            kind: sylvander_protocol::InteractionTimeoutKind::Plan,
            subject_id,
            timeout_secs: 300,
            recovery: sylvander_protocol::TimeoutRecovery::RetryRequest,
        } if subject_id == "plan-1"
    ));
}

#[test]
fn configured_pricing_calculates_nano_usd_and_requires_cache_rates() {
    let pricing = sylvander_protocol::ModelPricing {
        input_usd_micros_per_million: 3_000_000,
        output_usd_micros_per_million: 15_000_000,
        cache_write_usd_micros_per_million: None,
        cache_read_usd_micros_per_million: Some(300_000),
    };
    let mut usage = sylvander_llm_anthropic::api::types::Usage {
        input_tokens: 1_000,
        output_tokens: 100,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: Some(10_000),
    };
    assert_eq!(usage_cost_nano_usd(pricing, &usage), Some(7_500_000));
    usage.cache_creation_input_tokens = Some(1);
    assert_eq!(usage_cost_nano_usd(pricing, &usage), None);
}

#[tokio::test]
async fn agent_run_is_cloneable() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let run2 = run.clone();
    assert_eq!(run.id(), run2.id());
}

#[tokio::test]
async fn agent_run_previews_and_rolls_back_journaled_write() {
    use crate::tool::Tool;
    let workspace = tempfile::TempDir::new().unwrap();
    let journal = tempfile::TempDir::new().unwrap();
    let file = workspace.path().join("file.txt");
    std::fs::write(&file, "before").unwrap();
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .workspace_journal(journal.path())
        .build()
        .unwrap();
    let session_id = run
        .join_session(SessionMetadata {
            workspace: workspace.path().into(),
            ..test_metadata()
        })
        .await;
    let context = ToolContext::new(
        sylvander_protocol::SessionContext::new("user-1", "test-agent", session_id.clone())
            .with_trace_id("turn-1"),
    )
    .with_fs_root(workspace.path())
    .with_capability(Cap::Write)
    .with_workspace_journal(run.inner.workspace_journal.clone().unwrap());
    crate::tools::WriteTool::new(workspace.path())
        .execute(
            &context,
            serde_json::json!({"file_path":"file.txt","content":"after"}),
        )
        .await
        .unwrap();

    let preview = run.preview_workspace_rollback(&session_id).await.unwrap();
    assert_eq!(preview.files, vec!["file.txt"]);
    run.rollback_workspace_latest(&session_id, &preview.turn_id)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(file).unwrap(), "before");
}

#[tokio::test]
async fn runtime_model_selection_is_catalog_backed_and_capability_checked() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let thinking = ModelInfo::builder()
        .id("thinking-model")
        .context_window(200_000)
        .max_output_tokens(32_000)
        .capability(ModelCapabilities::EXTENDED_THINKING)
        .build()
        .expect("model");
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .available_models(vec![thinking])
        .model_lifecycles(HashMap::from([(
            "thinking-model".into(),
            sylvander_protocol::ModelLifecycle::Deprecated {
                replacement: Some("claude-sonnet-5-20260601".into()),
            },
        )]))
        .build()
        .expect("build");

    let initial = run.runtime_model_info().await;
    assert_eq!(initial.current_model, "claude-sonnet-5-20260601");
    assert_eq!(initial.models.len(), 2);
    assert!(matches!(
        initial
            .models
            .iter()
            .find(|model| model.id == "thinking-model")
            .map(|model| &model.lifecycle),
        Some(sylvander_protocol::ModelLifecycle::Deprecated {
            replacement: Some(replacement)
        }) if replacement == "claude-sonnet-5-20260601"
    ));
    let selected = run
        .select_model("thinking-model", sylvander_protocol::ReasoningEffort::High)
        .await
        .expect("select");
    assert_eq!(selected.current_model, "thinking-model");
    assert_eq!(
        selected.reasoning_effort,
        sylvander_protocol::ReasoningEffort::High
    );
    assert!(
        run.select_model(
            "claude-sonnet-5-20260601",
            sylvander_protocol::ReasoningEffort::Low,
        )
        .await
        .is_err()
    );
    assert_eq!(
        run.runtime_model_info().await.current_model,
        "thinking-model"
    );
}

#[tokio::test]
async fn context_report_separates_window_usage_from_cumulative_accounting() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let session_id = run.join_session(test_metadata()).await;
    run.inner
        .sessions
        .write()
        .await
        .get_mut(&session_id)
        .expect("session")
        .append_user_message(sylvander_llm_anthropic::api::types::MessageParam::user(
            "hello",
        ));
    run.inner.context_usage.write().await.insert(
        session_id.clone(),
        ContextUsage {
            used: 1_250,
            cache_read: 900,
            cache_write: 120,
        },
    );

    let report = run.context_report(Some(&session_id)).await;
    assert_eq!(report.used_tokens, 1_250);
    assert_eq!(report.cache_read_tokens, 900);
    assert_eq!(report.cache_write_tokens, 120);
    assert_eq!(
        report.remaining_tokens,
        report.context_window.saturating_sub(1_250)
    );
    assert!(report.sources.iter().any(|source| {
        source.kind == sylvander_protocol::ContextSourceKind::Conversation && source.items == 1
    }));
}

#[tokio::test]
async fn runtime_permissions_are_validated_against_operator_capabilities() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    assert_eq!(
        run.permission_profile().await,
        sylvander_protocol::PermissionProfile::default()
    );
    let restricted = sylvander_protocol::PermissionProfile {
        file_access: sylvander_protocol::FileAccess::ReadOnly,
        network_access: sylvander_protocol::NetworkAccess::Denied,
        approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
    };
    assert_eq!(
        run.select_permissions(restricted.clone()).await.unwrap(),
        restricted
    );
    assert!(
        run.select_permissions(sylvander_protocol::PermissionProfile {
            approval_policy: sylvander_protocol::ApprovalPolicy::Ask,
            ..Default::default()
        })
        .await
        .is_err()
    );
}

#[test]
fn permission_profile_builds_a_workspace_scoped_tool_context() {
    let metadata = test_metadata();
    let context = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: None,
            workspace_executors: &HashMap::from([(
                "local".to_owned(),
                Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>,
            )]),
        },
        &AgentId::new("agent"),
        &SessionId::new("session"),
        &sylvander_protocol::PermissionProfile {
            file_access: sylvander_protocol::FileAccess::ReadOnly,
            network_access: sylvander_protocol::NetworkAccess::Allowed,
            approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
        },
        true,
        None,
        Some("turn-1"),
    );
    assert_eq!(
        context.surface.fs_root.as_deref(),
        Some(metadata.workspace.as_path())
    );
    assert!(context.has_cap(Cap::Read));
    assert!(context.has_cap(Cap::Git));
    assert!(!context.has_cap(Cap::Write));
    assert!(context.has_cap(Cap::Network));
    assert!(context.host_allowed("example.com"));
    assert!(context.has_cap(Cap::MemoryRead));
    assert_eq!(context.user_id().0, metadata.user_id);
    assert_eq!(context.session.request.trace_id.as_deref(), Some("turn-1"));
}

#[test]
fn builder_registers_local_and_injected_workspace_executors() {
    let (spec, client) = test_spec_and_client();
    let remote: Arc<dyn WorkspaceExecutor> = Arc::new(MarkerWorkspaceExecutor::new(b"remote"));
    let run = AgentRun::builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .workspace_executor("ssh:build", remote.clone())
        .build()
        .expect("build");

    assert!(run.inner.workspace_executors.contains_key("local"));
    assert!(Arc::ptr_eq(
        run.inner.workspace_executors.get("ssh:build").unwrap(),
        &remote
    ));
}

#[tokio::test]
async fn turn_context_resolves_the_effective_execution_target() {
    let metadata = test_metadata();
    let effective = remote_effective_config("ssh:build", "/remote/project");
    let remote = Arc::new(MarkerWorkspaceExecutor::new(b"remote"));
    let executors = HashMap::from([(
        "ssh:build".to_owned(),
        remote.clone() as Arc<dyn WorkspaceExecutor>,
    )]);
    let context = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: Some(&effective),
            workspace_executors: &executors,
        },
        &AgentId::new("agent"),
        &SessionId::new("session"),
        &sylvander_protocol::PermissionProfile::default(),
        false,
        None,
        Some("turn-1"),
    );

    let bytes = context
        .executor
        .read_file(&context.execution_target, "README.md")
        .await
        .unwrap();
    assert_eq!(bytes, b"remote");
    assert_eq!(context.execution_target.id, "ssh:build");
    assert_eq!(
        context.execution_target.workspace_path,
        Path::new("/remote/project")
    );
    assert_eq!(
        remote.reads.lock().unwrap().as_slice(),
        &[context.execution_target]
    );
}

#[tokio::test]
async fn executor_resolution_is_rebuilt_after_agent_restart() {
    let metadata = test_metadata();
    let effective = remote_effective_config("container:dev", "/workspace");
    let old: Arc<dyn WorkspaceExecutor> = Arc::new(MarkerWorkspaceExecutor::new(b"old"));
    let new: Arc<dyn WorkspaceExecutor> = Arc::new(MarkerWorkspaceExecutor::new(b"new"));
    let (spec, client) = test_spec_and_client();
    let before_restart = AgentRun::builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .workspace_executor("container:dev", old)
        .build()
        .unwrap();
    drop(before_restart);
    let (spec, client) = test_spec_and_client();
    let after_restart = AgentRun::builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .workspace_executor("container:dev", new)
        .build()
        .unwrap();
    let permissions = sylvander_protocol::PermissionProfile::default();
    let context_after_restart = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: Some(&effective),
            workspace_executors: &after_restart.inner.workspace_executors,
        },
        &AgentId::new("agent"),
        &SessionId::new("restored-session"),
        &permissions,
        false,
        None,
        Some("new-turn"),
    );

    let bytes = context_after_restart
        .executor
        .read_file(&context_after_restart.execution_target, "Cargo.toml")
        .await
        .unwrap();
    assert_eq!(bytes, b"new");
}

#[tokio::test]
async fn effective_workspace_mounts_route_file_operations_by_logical_reference() {
    let task = tempfile::tempdir().unwrap();
    let dependency = tempfile::tempdir().unwrap();
    std::fs::write(task.path().join("task.txt"), "task").unwrap();
    std::fs::write(dependency.path().join("lib.txt"), "dependency").unwrap();
    let metadata = test_metadata();
    let mut effective = remote_effective_config("local", task.path().to_str().unwrap());
    effective.workspace_mounts = vec![
        sylvander_protocol::SessionWorkspaceMount {
            reference: "task".into(),
            role: sylvander_protocol::WorkspaceMountRole::Task,
            binding: sylvander_protocol::SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: task.path().into(),
                read_only: false,
                instruction_focus: None,
            },
            capabilities: sylvander_protocol::WorkspaceCapabilityPolicy {
                read: true,
                write: true,
                command: true,
                git: true,
            },
        },
        sylvander_protocol::SessionWorkspaceMount {
            reference: "shared".into(),
            role: sylvander_protocol::WorkspaceMountRole::Dependency,
            binding: sylvander_protocol::SessionWorkspaceBinding {
                execution_target: "local".into(),
                path: dependency.path().into(),
                read_only: true,
                instruction_focus: None,
            },
            capabilities: sylvander_protocol::WorkspaceCapabilityPolicy {
                read: true,
                git: true,
                ..Default::default()
            },
        },
    ];
    let executors = [(
        "local".into(),
        Arc::new(crate::workspace_executor::LocalExecutor) as Arc<dyn WorkspaceExecutor>,
    )]
    .into_iter()
    .collect();
    let context = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: Some(&effective),
            workspace_executors: &executors,
        },
        &AgentId::new("agent"),
        &SessionId::new("session"),
        &sylvander_protocol::PermissionProfile::default(),
        false,
        None,
        None,
    );

    assert_eq!(
        context
            .executor
            .read_file(&context.execution_target, "task.txt")
            .await
            .unwrap(),
        b"task"
    );
    assert_eq!(
        context
            .executor
            .read_file(&context.execution_target, "@shared/lib.txt")
            .await
            .unwrap(),
        b"dependency"
    );
    assert!(
        context
            .executor
            .write_file(&context.execution_target, "@shared/nope.txt", b"x")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unknown_execution_target_is_explicitly_unavailable() {
    let metadata = test_metadata();
    let effective = remote_effective_config("ssh:missing", "/remote/project");
    let context = tool_context_for_permissions(
        ToolSessionExecution {
            metadata: &metadata,
            effective_config: Some(&effective),
            workspace_executors: &HashMap::new(),
        },
        &AgentId::new("agent"),
        &SessionId::new("session"),
        &sylvander_protocol::PermissionProfile::default(),
        false,
        None,
        None,
    );

    let error = context
        .executor
        .read_file(&context.execution_target, "README.md")
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        crate::workspace_executor::WorkspaceExecutorError::Unavailable(target)
            if target == "ssh:missing"
    ));
}

#[test]
fn user_workspace_precedes_agent_workspace_and_agent_fallback_keeps_read_only() {
    let user = sylvander_protocol::SessionWorkspaceBinding {
        execution_target: "local".into(),
        path: "/user".into(),
        read_only: false,
        instruction_focus: None,
    };
    let agent = sylvander_protocol::SessionWorkspaceBinding {
        execution_target: "ssh:agent".into(),
        path: "/agent".into(),
        read_only: true,
        instruction_focus: None,
    };
    assert_eq!(
        select_workspace_binding(Some(&user), Some(&agent)),
        Some(&user)
    );
    let selected = select_workspace_binding(None, Some(&agent)).unwrap();
    assert_eq!(selected.execution_target, "ssh:agent");
    assert!(selected.read_only);
}

#[tokio::test]
async fn interrupt_is_scoped_to_the_selected_session() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let session_a = SessionId::new("session-a");
    let session_b = SessionId::new("session-b");
    let (interrupt_a, interrupted_a) = oneshot::channel();
    let (interrupt_b, mut interrupted_b) = oneshot::channel();
    run.inner.active_turns.lock().await.insert(
        session_a.clone(),
        ActiveTurn {
            id: uuid::Uuid::new_v4(),
            interrupt: interrupt_a,
        },
    );
    run.inner.active_turns.lock().await.insert(
        session_b,
        ActiveTurn {
            id: uuid::Uuid::new_v4(),
            interrupt: interrupt_b,
        },
    );

    run.inner.interrupt_turn(&session_a).await;

    assert!(interrupted_a.await.is_ok());
    assert!(matches!(
        interrupted_b.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn interactive_decisions_are_scoped_when_ids_collide_across_sessions() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(bus.clone())
        .build()
        .expect("build");
    let session_a = SessionId::new("session-a");
    let session_b = SessionId::new("session-b");
    let (approval_a_tx, approval_a_rx) = oneshot::channel();
    let (approval_b_tx, mut approval_b_rx) = oneshot::channel();
    let (answer_a_tx, answer_a_rx) = oneshot::channel();
    let (answer_b_tx, mut answer_b_rx) = oneshot::channel();
    let (plan_a_tx, plan_a_rx) = oneshot::channel();
    let (plan_b_tx, mut plan_b_rx) = oneshot::channel();

    for (session, approval, answer, plan) in [
        (&session_a, approval_a_tx, answer_a_tx, plan_a_tx),
        (&session_b, approval_b_tx, answer_b_tx, plan_b_tx),
    ] {
        let grant = ApprovalGrantContext::new(
            "user",
            AgentId::new("agent"),
            format!("sha256:{}", "1".repeat(64)),
            format!("sha256:{}", "2".repeat(64)),
        )
        .key_for(&ToolUseRequest {
            call_id: "shared-id".into(),
            tool_name: "write".into(),
            input: serde_json::json!({"path": "shared"}),
        });
        run.inner.pending_approvals.lock().await.insert(
            (session.clone(), "shared-id".into()),
            PendingApproval {
                session_id: session.clone(),
                grant,
                persistent_identity_authorized: true,
                allowed_scopes: vec![sylvander_protocol::ApprovalScope::Once],
                sender: approval,
            },
        );
        run.inner.pending_answers.lock().await.insert(
            (session.clone(), "shared-id".into()),
            PendingAnswer {
                session_id: session.clone(),
                sender: answer,
            },
        );
        run.inner.pending_plans.lock().await.insert(
            (session.clone(), "shared-id".into()),
            PendingPlan {
                session_id: session.clone(),
                sender: plan,
            },
        );
    }

    let inbox = bus.subscribe(run.subscription_filter()).await.unwrap();
    let task = tokio::spawn(run.run(inbox));
    for kind in [
        SystemMessage::ApproveTool {
            call_id: "shared-id".into(),
            approved: false,
            scope: sylvander_protocol::ApprovalScope::Once,
            reason: Some("session A rejected".into()),
        },
        SystemMessage::AnswerQuestion {
            call_id: "shared-id".into(),
            answer: "session A answer".into(),
        },
        SystemMessage::ResolvePlan {
            plan_id: "shared-id".into(),
            decision: sylvander_protocol::PlanDecision::Approved,
        },
    ] {
        bus.publish(BusMessage {
            session_id: session_a.clone(),
            sender: crate::bus::Sender::System,
            recipient: crate::bus::Recipient::Agent(AgentId::new("test-agent")),
            kind: MessageKind::System(kind),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: crate::session::now_secs(),
            id: crate::bus::MessageId::new(),
        })
        .await
        .unwrap();
    }

    assert!(matches!(
        approval_a_rx.await.unwrap(),
        ApprovalDecision::Rejected { reason } if reason == "session A rejected"
    ));
    assert_eq!(answer_a_rx.await.unwrap(), ["session A answer"]);
    assert_eq!(plan_a_rx.await.unwrap(), crate::bus::PlanDecision::Approved);
    assert!(matches!(
        approval_b_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        answer_b_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        plan_b_rx.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    task.abort();
}

#[tokio::test]
async fn durable_session_history_restores_into_agent_context() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let agent_id = spec.id.clone();
    let store: Arc<dyn SessionStore> = Arc::new(
        crate::session_store::SqliteSessionStore::open_in_memory()
            .await
            .expect("store"),
    );
    let session_id = SessionId::new("durable-session");
    let metadata = test_metadata();
    store
        .save(&StoredSession::new(
            session_id.clone(),
            metadata.name.clone(),
            SessionLifetime::Persistent,
            metadata.clone(),
            vec![agent_id.clone()],
        ))
        .await
        .expect("save session");
    let caller = sylvander_protocol::SessionContext::new(
        metadata.user_id.clone(),
        agent_id,
        session_id.clone(),
    );
    store
        .append_message(
            &caller,
            &session_id,
            StoredMessageRole::User,
            serde_json::to_value(sylvander_llm_anthropic::api::types::MessageParam::user(
                "remember me",
            ))
            .expect("serialize"),
            None,
            None,
            None,
        )
        .await
        .expect("append");

    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .session_store(store)
        .build()
        .expect("build");
    let restored = run
        .inner
        .restore_session_context(&session_id, &metadata)
        .await;

    assert_eq!(restored.len(), 1);
}

#[tokio::test]
async fn direct_join_persists_an_auditable_effective_configuration() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let resolver = Arc::new(
        crate::prompt::PromptResolver::new(
            "agent:test-agent@1".into(),
            spec.persona.system_prompt.clone(),
            Vec::new(),
            None,
            false,
        )
        .expect("resolver"),
    );
    let store: Arc<dyn SessionStore> = Arc::new(
        crate::session_store::SqliteSessionStore::open_in_memory()
            .await
            .expect("store"),
    );
    let session_id = SessionId::new("direct-session");
    let metadata = test_metadata();
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .session_store(store.clone())
        .prompt_resolver(resolver)
        .build()
        .expect("build");

    run.inner
        .restore_session_context(&session_id, &metadata)
        .await;

    let stored = store.get(&session_id).await.unwrap().unwrap();
    let effective = stored
        .effective_config
        .expect("direct session must snapshot runtime defaults");
    assert_eq!(effective.agent_id, run.id().clone());
    assert!(!effective.prompt_manifest.layers.is_empty());
    assert_eq!(effective.user_workspace.unwrap().path, metadata.workspace);
    assert_eq!(
        effective.provenance.model.kind,
        sylvander_protocol::SessionConfigSourceKind::AgentDefault
    );
}

#[tokio::test]
async fn compacted_history_replaces_runtime_and_durable_active_history() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let agent_id = spec.id.clone();
    let store: Arc<dyn SessionStore> = Arc::new(
        crate::session_store::SqliteSessionStore::open_in_memory()
            .await
            .expect("store"),
    );
    let session_id = SessionId::new("compact-session");
    let metadata = test_metadata();
    store
        .save(&StoredSession::new(
            session_id.clone(),
            metadata.name.clone(),
            SessionLifetime::Persistent,
            metadata.clone(),
            vec![agent_id.clone()],
        ))
        .await
        .expect("save");
    let caller = sylvander_protocol::SessionContext::new(
        metadata.user_id.clone(),
        agent_id,
        session_id.clone(),
    );
    for index in 0..6 {
        store
            .append_message(
                &caller,
                &session_id,
                StoredMessageRole::User,
                serde_json::to_value(sylvander_llm_anthropic::api::types::MessageParam::user(
                    format!("message {index}"),
                ))
                .expect("serialize"),
                None,
                None,
                None,
            )
            .await
            .expect("append");
    }
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .session_store(store.clone())
        .build()
        .expect("build");
    run.inner.sessions.write().await.insert(
        session_id.clone(),
        SessionContext::new(session_id.clone(), metadata),
    );
    let history = vec![
        sylvander_llm_anthropic::api::types::MessageParam::user(
            "[Earlier conversation summary]\nimportant decisions",
        ),
        sylvander_llm_anthropic::api::types::MessageParam::user("recent one"),
        sylvander_llm_anthropic::api::types::MessageParam::user("recent two"),
    ];
    let layers = vec![crate::compress::layer::LayerReport {
        name: "auto_compact".into(),
        removed_count: 4,
        freed_tokens: 500,
        details: Some(serde_json::json!({"summary": "important decisions"})),
        ..Default::default()
    }];
    run.inner
        .apply_compacted_history(&session_id, &history, &layers)
        .await
        .expect("replace history");

    assert_eq!(
        run.get_session(&session_id).await.expect("session").len(),
        3
    );
    let active = store
        .read_history(&caller, &session_id, false, None)
        .await
        .expect("active history");
    assert_eq!(active.len(), 3);
    assert!(
        active[0]
            .content
            .to_string()
            .contains("important decisions")
    );
}

#[tokio::test]
async fn memory_is_infrastructure_not_tool() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let store = Arc::new(InMemoryMemoryStore::new());
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .memory(store)
        .user_profile_provider(Arc::new(FixedUserProfile(profile_with_learning(false))))
        .build()
        .expect("build");
    let tools = run.memory_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "read_memory");
}

#[tokio::test]
async fn session_capability_is_bound_to_one_run() {
    let (spec_a, client_a) = test_spec_and_client();
    let (run_a, issuer_a) = AgentRun::builder(spec_a, client_a)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build_with_session_issuer()
        .expect("build A");
    let (spec_b, client_b) = test_spec_and_client();
    let (run_b, _) = AgentRun::builder(spec_b, client_b)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build_with_session_issuer()
        .expect("build B");
    let session_id = SessionId::new("session-a");
    let lease = issuer_a
        .issue(session_id, test_metadata())
        .expect("issue lease");

    let error = run_b
        .attach_authenticated_session(lease)
        .await
        .expect_err("foreign run must reject lease");
    assert!(matches!(error, AgentRunError::Authentication(_)));
    assert!(run_a.list_sessions().await.is_empty());
    assert!(run_b.list_sessions().await.is_empty());
}

#[test]
fn session_issuer_rejects_control_characters_before_admission() {
    let (spec, client) = test_spec_and_client();
    let (_, issuer) = AgentRun::builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .build_with_session_issuer()
        .expect("build");
    let error = issuer
        .issue(
            SessionId::new("sentinel-session"),
            SessionMetadata {
                user_id: "victim\nforged".into(),
                ..test_metadata()
            },
        )
        .err()
        .expect("unsafe identity must fail");
    assert!(matches!(error, AgentRunError::Authentication(_)));
}

#[tokio::test]
async fn raw_session_presence_has_no_trusted_memory_identity() {
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(Arc::new(InProcessMessageBus::new()))
        .memory(Arc::new(InMemoryMemoryStore::new()))
        .build()
        .expect("build");
    let session_id = SessionId::new("raw-bus-session");
    run.inner.sessions.write().await.insert(
        session_id.clone(),
        SessionContext::new(session_id.clone(), test_metadata()),
    );

    assert!(matches!(
        run.memory_context_for_session(&session_id).await,
        Err(MemoryStoreError::AccessDenied)
    ));
}

#[tokio::test]
async fn remember_is_system_driven() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let store = Arc::new(InMemoryMemoryStore::new());
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .memory(store)
        .user_profile_provider(Arc::new(FixedUserProfile(profile_with_learning(false))))
        .build()
        .expect("build");
    let session_id = run.join_session(test_metadata()).await;
    let session = run.authenticated_session_for_test(session_id);
    run.remember(&session, "User prefers dark mode", &["preference"])
        .await
        .expect("remember");
    let results = run
        .recall(
            &session,
            "dark mode",
            crate::tools::memory::MemoryFilter::default(),
        )
        .await
        .expect("search");
    assert_eq!(results.len(), 1);
}

#[tokio::test]
async fn remember_derives_identity_from_attached_session() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let store = Arc::new(InMemoryMemoryStore::new());
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .memory(store)
        .user_profile_provider(Arc::new(FixedUserProfile(profile_with_learning(false))))
        .build()
        .expect("build");
    let session_id = run
        .join_session(SessionMetadata {
            user_id: "actual-user".into(),
            ..test_metadata()
        })
        .await;
    let session = run.authenticated_session_for_test(session_id);
    let entry = run.remember(&session, "caller-owned", &[]).await.unwrap();

    assert_eq!(
        entry.owner,
        crate::tools::memory::MemoryOwner::Relationship {
            user_id: sylvander_protocol::types::UserId::new("actual-user"),
            agent_id: run.id().clone(),
        }
    );
    assert_eq!(
        run.recall(
            &session,
            "caller-owned",
            crate::tools::memory::MemoryFilter::default(),
        )
        .await
        .unwrap()
        .len(),
        1
    );
}

#[tokio::test]
async fn remember_denies_opt_out_missing_and_unavailable_profile_authority() {
    let providers = [
        Some(Arc::new(FixedUserProfile(profile_with_learning(true)))
            as Arc<dyn crate::user_profile_provider::UserProfileProvider>),
        Some(Arc::new(UnavailableUserProfile)
            as Arc<dyn crate::user_profile_provider::UserProfileProvider>),
        None,
    ];
    for provider in providers {
        let (spec, client) = test_spec_and_client();
        let mut builder = AgentRun::builder(spec, client)
            .bus(Arc::new(InProcessMessageBus::new()))
            .memory(Arc::new(InMemoryMemoryStore::new()));
        if let Some(provider) = provider {
            builder = builder.user_profile_provider(provider);
        }
        let run = builder.build().unwrap();
        let session_id = run.join_session(test_metadata()).await;
        let session = run.authenticated_session_for_test(session_id);

        assert!(matches!(
            run.remember(&session, "must not persist", &[]).await,
            Err(MemoryStoreError::AccessDenied)
        ));
        assert!(
            run.recall(
                &session,
                "must not persist",
                crate::tools::memory::MemoryFilter::default(),
            )
            .await
            .unwrap()
            .is_empty()
        );
    }
}

#[tokio::test]
async fn remember_fails_without_memory_configured() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let session_id = run.join_session(test_metadata()).await;
    let session = run.authenticated_session_for_test(session_id);
    let err = run.remember(&session, "something", &[]).await.unwrap_err();
    assert!(err.to_string().contains("no memory store"));
}

#[tokio::test]
async fn memory_tools_empty_without_memory_configured() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    assert!(run.memory_tools().is_empty());
}

#[test]
fn typed_attachments_become_provider_content_blocks() {
    let message = BusMessage::user_chat_with_attachments(
        SessionId::new("s1"),
        "u1",
        "review this",
        vec![crate::bus::MessageAttachment {
            id: "a1".into(),
            kind: crate::bus::AttachmentKind::File,
            name: "src/main.rs".into(),
            mime_type: "text/x-rust".into(),
            content: crate::bus::AttachmentContent::Text {
                text: "fn main() {}".into(),
            },
            byte_count: 12,
        }],
    );
    let value = serde_json::to_value(AgentRunInner::message_to_param(&message)).expect("json");
    let content = value["content"].as_array().expect("content blocks");
    assert_eq!(content.len(), 2);
    assert!(content[1]["text"].as_str().unwrap().contains("src/main.rs"));
}

#[tokio::test]
async fn join_and_leave_session() {
    let bus = Arc::new(InProcessMessageBus::new());
    let (spec, client) = test_spec_and_client();
    let run = AgentRun::builder(spec, client)
        .bus(bus)
        .build()
        .expect("build");
    let sid = run.join_session(test_metadata()).await;
    assert_eq!(run.list_sessions().await.len(), 1);
    run.leave_session(&sid).await;
    assert!(run.list_sessions().await.is_empty());
}

#[tokio::test]
async fn subscription_filter_matches_agent_and_broadcast() {
    let bus = Arc::new(InProcessMessageBus::new());
    let spec = AgentSpec::builder()
        .id("filter-test")
        .name("Filter Test")
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec");
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .expect("client");
    let run = AgentRun::builder(spec, client)
        .bus(bus.clone())
        .build()
        .expect("build");
    let filter = run.subscription_filter();
    let agent_id = AgentId::new("filter-test");
    assert!(filter.matches(&BusMessage {
        recipient: Recipient::Agent(agent_id.clone()),
        ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi")
    }));
    assert!(filter.matches(&BusMessage {
        recipient: Recipient::Broadcast,
        ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi")
    }));
    assert!(!filter.matches(&BusMessage {
        recipient: Recipient::Agent(AgentId::new("other")),
        ..BusMessage::user_chat(SessionId::new("s1"), "u1", "hi")
    }));
}
