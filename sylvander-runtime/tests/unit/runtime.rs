use super::*;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use sylvander_agent::tool::Tool;
use sylvander_agent::tool_context::{Cap, ToolContext};
use sylvander_agent::tools::memory::MemoryFilter;
use sylvander_agent::tools::{CommandTool, MemoryActorKind, MemoryAppend, MemoryProvenanceSource};
use sylvander_agent::workspace_executor::{WorkspaceExecutor, WorkspaceTarget};
use sylvander_protocol::SessionContext;
use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn host_backed_targets_support_local_worktree_isolation() {
    let targets = HashMap::from([
        (
            "local".into(),
            config::ExecutionTransportConfig::Local { root: None },
        ),
        (
            "container".into(),
            config::ExecutionTransportConfig::Container {
                runtime: "docker".into(),
                image: "rust:latest".into(),
                resources: config::ContainerResourceSettings::default(),
            },
        ),
        (
            "ssh".into(),
            config::ExecutionTransportConfig::Ssh {
                host: "host".into(),
                port: 22,
                user: "user".into(),
                credential: config::SecretRef::Env {
                    name: "SSH_KEY".into(),
                },
                known_hosts: PathBuf::from("/tmp/sylvander-known-hosts"),
                control_path: PathBuf::from("/tmp/sylvander-ssh-control"),
                worktree_root: PathBuf::from("/tmp/sylvander-worktrees"),
            },
        ),
    ]);

    assert!(execution_target_supports_host_worktree(&targets, "local"));
    assert!(execution_target_supports_host_worktree(
        &targets,
        "container"
    ));
    assert!(!execution_target_supports_host_worktree(&targets, "ssh"));
}

#[test]
fn additional_writable_remote_mount_requires_an_independent_worktree_transaction() {
    use sylvander_protocol::{
        SessionWorkspaceBinding, SessionWorkspaceMount, WorkspaceCapabilityPolicy,
        WorkspaceMountRole,
    };

    let binding = |path: &str, read_only| SessionWorkspaceBinding {
        execution_target: "ssh".into(),
        path: path.into(),
        read_only,
        instruction_focus: None,
    };
    let mounts = vec![
        SessionWorkspaceMount {
            reference: "task".into(),
            role: WorkspaceMountRole::Task,
            binding: binding("/task", false),
            capabilities: WorkspaceCapabilityPolicy {
                read: true,
                write: true,
                command: true,
                git: true,
            },
        },
        SessionWorkspaceMount {
            reference: "artifacts".into(),
            role: WorkspaceMountRole::Artifact,
            binding: binding("/artifacts", false),
            capabilities: WorkspaceCapabilityPolicy {
                read: true,
                write: true,
                command: false,
                git: false,
            },
        },
    ];

    // A production service identifies configured SSH managers as remote. Use
    // the pure policy predicate here so this negative test never opens SSH.
    let error =
        ensure_remote_mutation_mounts_are_transactional_with(&mounts, Some("task"), |target| {
            target == "ssh"
        })
        .expect_err("a second writable SSH mount must fail closed");
    assert!(error.contains("@artifacts"));

    let mut read_only_mounts = mounts;
    read_only_mounts[1].binding.read_only = true;
    read_only_mounts[1].capabilities.write = false;
    ensure_remote_mutation_mounts_are_transactional_with(
        &read_only_mounts,
        Some("task"),
        |target| target == "ssh",
    )
    .expect("a read-only auxiliary SSH mount needs no mutation transaction");
}

struct InstrumentedBus {
    inner: InProcessMessageBus,
    operations: std::sync::Mutex<Vec<&'static str>>,
    fail_subscribe: bool,
    fail_chat_publish: bool,
    fail_all_publish: bool,
}

impl InstrumentedBus {
    fn new(fail_subscribe: bool, fail_chat_publish: bool) -> Self {
        Self {
            inner: InProcessMessageBus::new(),
            operations: std::sync::Mutex::new(Vec::new()),
            fail_subscribe,
            fail_chat_publish,
            fail_all_publish: false,
        }
    }

    fn rejecting_publish() -> Self {
        Self {
            fail_all_publish: true,
            ..Self::new(false, false)
        }
    }

    fn operations(&self) -> Vec<&'static str> {
        self.operations.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl MessageBus for InstrumentedBus {
    async fn publish(&self, message: BusMessage) -> Result<(), sylvander_agent::bus::BusError> {
        let chat = matches!(message.kind, sylvander_agent::bus::MessageKind::Chat);
        self.operations
            .lock()
            .unwrap()
            .push(if chat { "publish_chat" } else { "publish" });
        if self.fail_all_publish || (chat && self.fail_chat_publish) {
            return Err(sylvander_agent::bus::BusError::SendFailed(
                "injected".into(),
            ));
        }
        self.inner.publish(message).await
    }

    async fn subscribe(
        &self,
        filter: SubscriptionFilter,
    ) -> Result<tokio::sync::mpsc::Receiver<BusMessage>, sylvander_agent::bus::BusError> {
        self.operations.lock().unwrap().push("subscribe");
        if self.fail_subscribe {
            return Err(sylvander_agent::bus::BusError::SubscribeFailed(
                "injected".into(),
            ));
        }
        self.inner.subscribe(filter).await
    }
}

struct BlockingChannel {
    started: Arc<Notify>,
    dropped: Arc<AtomicBool>,
}

struct ExitingChannel;

struct ReadyThenExitChannel {
    exit: Arc<Notify>,
}

struct RestartOnceChannel {
    attempts: Arc<AtomicUsize>,
}

fn channel_registration(instance_id: &str, channel: impl Channel + 'static) -> ChannelRegistration {
    ChannelRegistration::new(instance_id, Arc::new(channel))
}

#[async_trait::async_trait]
impl Channel for ExitingChannel {
    fn name(&self) -> &'static str {
        "exiting-test"
    }

    async fn run(self: Arc<Self>, _ctx: ChannelContext) {}
}

#[async_trait::async_trait]
impl Channel for ReadyThenExitChannel {
    fn name(&self) -> &'static str {
        "ready-then-exit-test"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        ctx.mark_ready();
        self.exit.notified().await;
    }
}

#[async_trait::async_trait]
impl Channel for RestartOnceChannel {
    fn name(&self) -> &'static str {
        "restart-once-test"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        ctx.mark_ready();
        if attempt > 0 {
            ctx.shutdown_requested().await;
        }
    }
}

struct DropSignal(Arc<AtomicBool>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Channel for BlockingChannel {
    fn name(&self) -> &'static str {
        "blocking-test"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let _drop_signal = DropSignal(self.dropped.clone());
        ctx.mark_ready();
        self.started.notify_one();
        ctx.shutdown_requested().await;
    }
}

fn test_spec(id: &str) -> AgentSpec {
    AgentSpec::builder()
        .id(id)
        .name(format!("Agent {id}"))
        .model_name("claude-sonnet-5-20260601")
        .build()
        .expect("spec")
}

fn test_client() -> AnthropicClient {
    AnthropicClient::builder()
        .api_key("test-key")
        .build()
        .expect("client")
}

fn test_metadata() -> SessionMetadata {
    SessionMetadata {
        workspace: PathBuf::from("/tmp"),
        name: "test".into(),
        user_id: "user-1".into(),
    }
}

fn configured_memory_test_config(
    directory: &tempfile::TempDir,
    agent_ids: &[&str],
) -> ServerConfig {
    let secret = directory.path().join("provider.key");
    std::fs::write(&secret, "0123456789abcdef0123456789abcdef").unwrap();
    let data_dir = directory.path().join("runtime-data");
    let anchor_dir = directory.path().join("integrity-anchor");
    std::fs::create_dir_all(&anchor_dir).unwrap();
    let agents = agent_ids.iter().fold(String::new(), |mut output, id| {
        use std::fmt::Write as _;
        write!(
            output,
            r#"
[[agents]]
[agents.spec]
id = "{id}"
name = "Agent {id}"
[agents.spec.model]
provider = "primary"
model_name = "model-a"
allowed_models = [{{ provider_id = "primary", model_id = "model-a" }}]
"#
        )
        .expect("write Agent test configuration");
        output
    });
    ServerConfig::from_toml(&format!(
        r#"
schema_version = 1
[server]
data_dir = "{}"

[server.memory_maintenance.integrity]
[server.memory_maintenance.integrity.key]
source = "file"
path = "{}"
[server.memory_maintenance.integrity.backend]
kind = "file"
anchor_path = "{}"

[[model_providers]]
id = "primary"
base_url = "https://models.invalid"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "model-a"
{agents}
"#,
        data_dir.display(),
        secret.display(),
        anchor_dir.join("anchor.json").display(),
        secret.display()
    ))
    .unwrap()
}

#[tokio::test]
async fn relative_database_paths_resolve_under_data_dir_and_reopen() {
    let directory = tempfile::tempdir().expect("temporary runtime directory");
    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config.server.session_db = Some(PathBuf::from("state/sessions.db"));
    config.server.memory_db = Some(PathBuf::from("state/memory.db"));
    config.server.user_profile_db = Some(PathBuf::from("state/user-profiles.db"));
    config.server.evidence.path = Some(PathBuf::from("state/evidence.db"));

    let runtime = Runtime::boot_config(config.clone())
        .await
        .expect("boot with durable relative database paths");
    runtime.shutdown().await.expect("first shutdown");
    drop(runtime);

    let data_dir = config.server.data_dir.as_ref().unwrap();
    for relative in [
        "state/sessions.db",
        "state/memory.db",
        "state/user-profiles.db",
        "state/evidence.db",
    ] {
        assert!(
            data_dir.join(relative).is_file(),
            "{relative} was not persisted beneath data_dir"
        );
    }

    let reopened = Runtime::boot_config(config)
        .await
        .expect("reopen every durable database");
    reopened.shutdown().await.expect("reopened shutdown");
}

#[tokio::test]
async fn configured_boot_rejects_named_shared_memory_databases() {
    for field in [
        "server.session_db",
        "server.memory_db",
        "server.user_profile_db",
        "server.evidence.path",
    ] {
        let directory = tempfile::tempdir().expect("temporary runtime directory");
        let mut config = configured_memory_test_config(&directory, &["assistant"]);
        let path = PathBuf::from("file:sylvander?mode=memory&cache=shared");
        match field {
            "server.session_db" => config.server.session_db = Some(path),
            "server.memory_db" => config.server.memory_db = Some(path),
            "server.user_profile_db" => config.server.user_profile_db = Some(path),
            "server.evidence.path" => config.server.evidence.path = Some(path),
            _ => unreachable!(),
        }
        let error = match Runtime::boot_config(config).await {
            Ok(runtime) => {
                runtime
                    .shutdown()
                    .await
                    .expect("unexpected runtime shutdown");
                panic!("{field} accepted a named shared-memory database")
            }
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(message.contains(field), "{field}: {message}");
        assert!(!message.contains("mode=memory"), "{field}: {message}");
    }
}

#[tokio::test]
async fn operational_readiness_tracks_evidence_and_guardian_background_failures() {
    let directory = tempfile::tempdir().expect("temporary runtime directory");
    let runtime = Runtime::boot_config(configured_memory_test_config(&directory, &[]))
        .await
        .expect("configured runtime");
    assert!(runtime.operational_snapshot().await.unwrap().ready);
    let guardian = runtime.guardian.as_ref().expect("Guardian runtime");
    guardian.set_last_error_for_test(true).await;
    let failed = runtime.operational_snapshot().await.unwrap();
    assert!(!failed.ready);
    assert_eq!(
        failed.health_issues,
        [RuntimeHealthIssue::GuardianSupervisor]
    );
    guardian.set_last_error_for_test(false).await;
    assert!(runtime.operational_snapshot().await.unwrap().ready);

    let recorder = runtime.evidence.as_ref().expect("production recorder");
    recorder.fail_next_record_for_test();
    runtime
        .bus
        .publish(BusMessage::user_chat(
            "evidence-failure".into(),
            "user",
            "private persistence failure sentinel",
        ))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if recorder.last_error().await.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("evidence failure must become visible");
    let failed = runtime.operational_snapshot().await.unwrap();
    assert!(!failed.ready);
    assert_eq!(failed.health_issues, [RuntimeHealthIssue::EvidenceRecorder]);

    let events_before_recovery = recorder.store().counts().await.unwrap().events;
    runtime
        .bus
        .publish(BusMessage::user_chat(
            "evidence-recovery".into(),
            "user",
            "recovery",
        ))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if recorder.store().counts().await.unwrap().events > events_before_recovery {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("later evidence writes must continue");
    let failed = runtime.operational_snapshot().await.unwrap();
    assert!(!failed.ready);
    assert_eq!(failed.health_issues, [RuntimeHealthIssue::EvidenceRecorder]);

    runtime.shutdown().await.unwrap();
}

fn git(repository: &std::path::Path, arguments: &[&str]) {
    let output = std::process::Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn wait_for_guardian_events(
    guardian: &GuardianRuntime,
    expected_events: i64,
    expected_records: i64,
) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let result = guardian
                .drain_once(sylvander_agent::session::now_secs())
                .await;
            if guardian.completed_event_count() == expected_events
                && guardian.canonical_record_count() == expected_records
            {
                return;
            }
            if let Err(error) = result {
                panic!("Guardian curation pass failed: {error}");
            }
            if let Some(error) = guardian.last_error().await {
                panic!("Guardian supervisor failed while awaiting curation: {error}");
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Guardian did not durably complete the expected curation events");
}

#[cfg(unix)]
fn fake_container_runtime(directory: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let executable = directory.join("fake-container-runtime");
    std::fs::write(
        &executable,
        r#"#!/bin/sh
if [ "$1" = rm ]; then exit 0; fi
[ "$1" = run ] || exit 90
shift
mount=
while [ "$#" -gt 0 ]; do
  case $1 in
    --rm|--network=none|--interactive|--read-only) shift ;;
    --name|--memory|--cpus|--pids-limit|--tmpfs|--security-opt|--cap-drop) shift 2 ;;
    --mount) mount=$2; shift 2 ;;
    --workdir) shift 2 ;;
    *) shift; break ;;
  esac
done
workspace=$(printf '%s' "$mount" | sed -n 's/.*source=\([^,]*\),target=.*/\1/p')
[ -n "$workspace" ] || exit 91
cd "$workspace" || exit 92
exec "$@"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    executable
}

#[tokio::test]
async fn coding_session_binds_effective_prompt_and_tools_to_one_worktree() {
    let model_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_worktree_tool",
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "tool-worktree-write",
                "name": "Write",
                "input": {"file_path": "routed.txt", "content": "worktree only\n"}
            }],
            "model": "model-a",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 4, "output_tokens": 4}
        })))
        .with_priority(1)
        .up_to_n_times(1)
        .expect(1)
        .mount(&model_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_worktree_done",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "written"}],
            "model": "model-a",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 8, "output_tokens": 2}
        })))
        .with_priority(2)
        .expect(1)
        .mount(&model_server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("project");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-b", "master"]);
    git(&repository, &["config", "user.email", "test@example.com"]);
    git(&repository, &["config", "user.name", "Sylvander Test"]);
    std::fs::write(repository.join("AGENTS.md"), "worktree instructions").unwrap();
    git(&repository, &["add", "AGENTS.md"]);
    git(&repository, &["commit", "-m", "initial"]);

    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config.model_providers[0].base_url = model_server.uri();
    config.model_providers[0].models[0].capabilities = vec!["tool_use".into()];
    config.agents[0].access.allow_authenticated = true;
    let runtime = Runtime::boot_config(config).await.unwrap();
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "workspace-owner",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "tui-local",
        "unix",
        "request-worktree",
    );
    let requested_workspace = sylvander_protocol::SessionWorkspaceBinding {
        execution_target: "local".into(),
        path: repository.clone(),
        read_only: false,
        instruction_focus: None,
    };
    let initial_overrides = SessionConfigOverrides {
        user_workspace: Some(requested_workspace.clone()),
        ..SessionConfigOverrides::default()
    };
    let created = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &boundary,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "isolated coding".into(),
            channel_id: Some("tui-local".into()),
            overrides: initial_overrides.clone(),
        },
    )
    .await
    .unwrap();
    let effective_workspace = created
        .effective
        .user_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    assert_ne!(effective_workspace, repository);
    assert!(effective_workspace.join("AGENTS.md").is_file());
    assert_eq!(
        created
            .effective
            .workspace_mounts
            .iter()
            .find(|mount| mount.reference == "task")
            .expect("the task projection must have one canonical mount")
            .binding
            .path,
        effective_workspace,
        "the real tool router must resolve the task mount inside the worktree"
    );

    let session_owner = runtime
        .session_store
        .get(&created.session_id)
        .await
        .unwrap()
        .unwrap()
        .metadata
        .user_id;
    runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap()
        .run
        .handle_message(sylvander_protocol::BusMessage::user_chat(
            created.session_id.clone(),
            session_owner,
            "write the routed file",
        ))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(effective_workspace.join("routed.txt")).unwrap(),
        "worktree only\n"
    );
    assert!(
        !repository.join("routed.txt").exists(),
        "the production tool-context router must never mutate the source checkout"
    );

    let stored = runtime
        .session_store
        .get(&created.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.metadata.workspace, effective_workspace);
    assert_eq!(stored.effective_config, Some(created.effective.clone()));
    let attached = runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap()
        .run
        .get_session(&created.session_id)
        .await
        .unwrap();
    assert_eq!(attached.metadata.workspace, effective_workspace);

    let updated = sylvander_channel::UiService::update_session_config(
        runtime.ui_service.as_ref(),
        &boundary,
        SessionConfigUpdateRequest {
            session_id: created.session_id.clone(),
            expected_revision: created.revision,
            overrides: SessionConfigOverrides {
                permissions: Some(sylvander_protocol::PermissionProfile {
                    file_access: sylvander_protocol::FileAccess::ReadOnly,
                    network_access: sylvander_protocol::NetworkAccess::Denied,
                    approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
                }),
                ..initial_overrides.clone()
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(
        updated.effective.user_workspace.unwrap().path,
        effective_workspace
    );

    let changed_workspace = directory.path().join("different");
    std::fs::create_dir(&changed_workspace).unwrap();
    let error = sylvander_channel::UiService::update_session_config(
        runtime.ui_service.as_ref(),
        &boundary,
        SessionConfigUpdateRequest {
            session_id: created.session_id.clone(),
            expected_revision: updated.revision,
            overrides: SessionConfigOverrides {
                user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
                    path: changed_workspace,
                    ..requested_workspace
                }),
                ..initial_overrides
            },
        },
    )
    .await
    .unwrap_err();
    assert!(
        error
            .message
            .contains("cannot change after session creation")
    );

    runtime
        .discard_coding_session(&created.session_id)
        .await
        .unwrap();
    let guardian = runtime
        .guardian
        .as_ref()
        .expect("configured Runtime must start Guardian");
    wait_for_guardian_events(guardian, 2, 0).await;
    assert_eq!(guardian.canonical_record_count(), 0);
}

#[tokio::test]
async fn coding_tool_review_and_resume_survive_runtime_restart() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("project");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-b", "master"]);
    git(&repository, &["config", "user.email", "test@example.com"]);
    git(&repository, &["config", "user.name", "Sylvander Test"]);
    std::fs::write(repository.join("tracked.txt"), "before\n").unwrap();
    git(&repository, &["add", "tracked.txt"]);
    git(&repository, &["commit", "-m", "initial"]);

    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config.agents[0].access.allow_authenticated = true;
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "workspace-owner",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "tui-local",
        "unix",
        "coding-lifecycle",
    );
    let overrides = SessionConfigOverrides {
        model: Some(ModelSelection {
            provider_id: "primary".into(),
            model_id: "model-a".into(),
        }),
        user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: repository.clone(),
            read_only: false,
            instruction_focus: None,
        }),
        ..SessionConfigOverrides::default()
    };

    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    let created = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &boundary,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "restartable coding".into(),
            channel_id: Some("tui-local".into()),
            overrides: overrides.clone(),
        },
    )
    .await
    .unwrap();
    let worktree = created
        .effective
        .user_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    assert_ne!(worktree, repository);

    let tool_context = ToolContext::new(SessionContext::new(
        UserId::new("workspace-owner"),
        AgentId::new("assistant"),
        created.session_id.clone(),
    ))
    .with_fs_root(&worktree)
    .with_capability(Cap::Spawn);
    let output = CommandTool::new()
            .execute(
                &tool_context,
                json!({"command": "printf 'accepted\\n' > tracked.txt; printf 'generated\\n' > generated.txt"}),
            )
            .await
            .unwrap();
    assert!(!output.is_error, "{}", output.content);
    assert_eq!(
        std::fs::read_to_string(repository.join("tracked.txt")).unwrap(),
        "before\n"
    );

    let diff = sylvander_channel::UiService::inspect_coding_session(
        runtime.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert!(diff.status.contains("M tracked.txt"));
    assert!(diff.status.contains("?? generated.txt"));
    assert!(diff.patch.contains("+accepted"));
    assert!(diff.patch.contains("+generated"));
    sylvander_channel::UiService::accept_coding_session(
        runtime.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(repository.join("tracked.txt")).unwrap(),
        "accepted\n"
    );
    runtime.shutdown().await.unwrap();
    drop(runtime);

    let restarted = Runtime::boot_config(config).await.unwrap();
    let resumed = sylvander_channel::UiService::session_config(
        restarted.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert_eq!(resumed.effective.agent_id, AgentId::new("assistant"));
    assert_eq!(resumed.effective.provider_id, "primary");
    assert_eq!(resumed.effective.model_id, "model-a");
    assert_eq!(resumed.effective.user_workspace.unwrap().path, worktree);
    assert_eq!(resumed.overrides, overrides);
    let clean = sylvander_channel::UiService::inspect_coding_session(
        restarted.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert!(clean.status.is_empty());
    assert!(clean.patch.is_empty());

    let output = CommandTool::new()
        .execute(
            &tool_context,
            json!({"command": "printf 'discarded\\n' > tracked.txt"}),
        )
        .await
        .unwrap();
    assert!(!output.is_error, "{}", output.content);
    let pending = sylvander_channel::UiService::inspect_coding_session(
        restarted.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert!(pending.patch.contains("+discarded"));
    sylvander_channel::UiService::discard_coding_session(
        restarted.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(repository.join("tracked.txt")).unwrap(),
        "accepted\n"
    );
    assert!(!worktree.exists());
    assert!(
        restarted
            .session_store
            .get(&created.session_id)
            .await
            .unwrap()
            .is_none()
    );
    restarted.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn container_coding_session_runs_in_worktree_and_survives_restart() {
    use crate::execution::container::ContainerExecutor;

    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("project");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-b", "master"]);
    git(&repository, &["config", "user.email", "test@example.com"]);
    git(&repository, &["config", "user.name", "Sylvander Test"]);
    std::fs::write(repository.join("tracked.txt"), "before\n").unwrap();
    git(&repository, &["add", "tracked.txt"]);
    git(&repository, &["commit", "-m", "initial"]);
    let container_runtime = fake_container_runtime(directory.path());

    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config.agents[0].access.allow_authenticated = true;
    config
        .execution_targets
        .push(config::ExecutionTargetConfig {
            id: "container".into(),
            transport: config::ExecutionTransportConfig::Container {
                runtime: container_runtime.display().to_string(),
                image: "sylvander/test:latest".into(),
                resources: config::ContainerResourceSettings::default(),
            },
        });
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "container-owner",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "tui-local",
        "unix",
        "container-coding",
    );
    let overrides = SessionConfigOverrides {
        user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "container".into(),
            path: repository.clone(),
            read_only: false,
            instruction_focus: None,
        }),
        ..SessionConfigOverrides::default()
    };

    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    let created = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &boundary,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "container coding".into(),
            channel_id: Some("tui-local".into()),
            overrides,
        },
    )
    .await
    .unwrap();
    let worktree = created
        .effective
        .user_workspace
        .as_ref()
        .unwrap()
        .path
        .clone();
    assert_ne!(worktree, repository);

    let executor = ContainerExecutor::new(&container_runtime, "sylvander/test:latest").unwrap();
    let target = WorkspaceTarget {
        id: "container".into(),
        workspace_path: worktree.clone(),
        read_only: false,
    };
    let output = executor
        .run_command(
            &target,
            "printf 'accepted\\n' > tracked.txt; printf 'generated\\n' > generated.txt",
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(output.success);
    let diff = sylvander_channel::UiService::inspect_coding_session(
        runtime.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert!(diff.patch.contains("+accepted"));
    assert!(diff.patch.contains("+generated"));
    sylvander_channel::UiService::accept_coding_session(
        runtime.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(repository.join("tracked.txt")).unwrap(),
        "accepted\n"
    );
    runtime.shutdown().await.unwrap();
    drop(runtime);

    let restarted = Runtime::boot_config(config).await.unwrap();
    let resumed = sylvander_channel::UiService::session_config(
        restarted.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert_eq!(resumed.effective.user_workspace.unwrap().path, worktree);
    executor
        .run_command(
            &target,
            "printf 'discarded\\n' > tracked.txt",
            std::time::Duration::from_secs(5),
        )
        .await
        .unwrap();
    sylvander_channel::UiService::discard_coding_session(
        restarted.ui_service.as_ref(),
        &boundary,
        &created.session_id,
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(repository.join("tracked.txt")).unwrap(),
        "accepted\n"
    );
    assert!(!worktree.exists());
    restarted.shutdown().await.unwrap();
}

fn ui_service_with_bus(runtime: &Runtime, bus: Arc<dyn MessageBus>) -> RuntimeUiService {
    RuntimeUiService {
        engine: runtime.ui_service.engine.clone(),
        bus,
        sessions: runtime.ui_service.sessions.clone(),
        agents: runtime.ui_service.agents.clone(),
        agent_registry: runtime.ui_service.agent_registry.clone(),
        revision_provider: runtime.ui_service.revision_provider.clone(),
        credential_resolver: runtime.ui_service.credential_resolver.clone(),
        credential_audit: runtime.ui_service.credential_audit.clone(),
        evidence: runtime.ui_service.evidence.clone(),
        evidence_run_id: runtime.ui_service.evidence_run_id.clone(),
        guardian: runtime.ui_service.guardian.clone(),
        identity_bindings: runtime.ui_service.identity_bindings.clone(),
        user_profiles: runtime.ui_service.user_profiles.clone(),
        worktrees: runtime.ui_service.worktrees.clone(),
        boundary: runtime.ui_service.boundary.clone(),
    }
}

#[tokio::test]
async fn authenticated_chat_submission_is_ordered_and_compensates_new_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config.agents[0].access.allow_authenticated = true;
    let runtime = Runtime::boot_config(config).await.unwrap();
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "channel-user",
            sylvander_protocol::AuthenticationMethod::PlatformIdentity,
        ),
        "channel-a",
        "test",
        "request-1",
    );
    let request = |existing_session| sylvander_channel::ExternalChatRequest {
        existing_session,
        agent_id: AgentId::new("assistant"),
        label: "authenticated chat".into(),
        overrides: SessionConfigOverrides::default(),
        text: "hello".into(),
        attachments: Vec::new(),
        external_meta: BTreeMap::from([("external_id".into(), "chat-1".into())]),
    };
    let agent = runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap();
    let initial_store = runtime.session_store.list_persistent().await.unwrap().len();
    let initial_engine = runtime
        .engine
        .list_sessions()
        .await
        .into_iter()
        .map(|session| session.id.0)
        .collect::<BTreeSet<_>>();
    let initial_agent = agent.run.list_sessions().await;

    let join_bus = Arc::new(InstrumentedBus::rejecting_publish());
    let mut join_failure = ui_service_with_bus(&runtime, Arc::new(InProcessMessageBus::new()));
    join_failure.engine = Arc::new(AgentRunEngine::new(join_bus.clone()));
    sylvander_channel::UiService::submit_chat(&join_failure, &boundary, request(None))
        .await
        .expect_err("engine attach failure must reject a new session");
    assert_eq!(join_bus.operations(), ["publish"]);
    assert!(join_failure.engine.list_sessions().await.is_empty());
    assert_eq!(
        runtime.session_store.list_persistent().await.unwrap().len(),
        initial_store
    );
    assert_eq!(agent.run.list_sessions().await, initial_agent);

    for (fail_subscribe, fail_publish, expected) in [
        (true, false, vec!["subscribe"]),
        (false, true, vec!["subscribe", "publish_chat"]),
    ] {
        let bus = Arc::new(InstrumentedBus::new(fail_subscribe, fail_publish));
        let service = ui_service_with_bus(&runtime, bus.clone());
        sylvander_channel::UiService::submit_chat(&service, &boundary, request(None))
            .await
            .expect_err("injected delivery failure must reject a new session");
        assert_eq!(bus.operations(), expected);
        assert_eq!(
            runtime.session_store.list_persistent().await.unwrap().len(),
            initial_store
        );
        assert_eq!(
            runtime
                .engine
                .list_sessions()
                .await
                .into_iter()
                .map(|session| session.id.0)
                .collect::<BTreeSet<_>>(),
            initial_engine
        );
        assert_eq!(agent.run.list_sessions().await, initial_agent);
    }

    let existing = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &boundary,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "existing".into(),
            channel_id: Some("channel-a".into()),
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    let failing_bus = Arc::new(InstrumentedBus::new(false, true));
    let failing_service = ui_service_with_bus(&runtime, failing_bus);
    sylvander_channel::UiService::submit_chat(
        &failing_service,
        &boundary,
        request(Some(existing.session_id.clone())),
    )
    .await
    .expect_err("existing-session publish failure must be reported");
    assert!(
        runtime
            .session_store
            .get(&existing.session_id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        runtime
            .engine
            .get_session(&existing.session_id)
            .await
            .is_some()
    );
    assert!(
        agent
            .run
            .list_sessions()
            .await
            .contains(&existing.session_id)
    );

    let selected_agent_bus = Arc::new(InstrumentedBus::new(false, false));
    let selected_agent_service = ui_service_with_bus(&runtime, selected_agent_bus);
    let mut existing_request = request(Some(existing.session_id.clone()));
    existing_request.agent_id = AgentId::new("different-channel-default");
    let mut selected_submission = sylvander_channel::UiService::submit_chat(
        &selected_agent_service,
        &boundary,
        existing_request,
    )
    .await
    .expect("the durable session Agent must override the channel creation default");
    let selected_chat = selected_submission.events.recv().await.unwrap();
    assert_eq!(
        selected_chat.recipient,
        Recipient::Agent(AgentId::new("assistant"))
    );

    let success_bus = Arc::new(InstrumentedBus::new(false, false));
    let success_service = ui_service_with_bus(&runtime, success_bus.clone());
    let mut submitted =
        sylvander_channel::UiService::submit_chat(&success_service, &boundary, request(None))
            .await
            .unwrap();
    assert_eq!(success_bus.operations(), ["subscribe", "publish_chat"]);
    let chat = submitted.events.recv().await.unwrap();
    assert!(matches!(chat.kind, sylvander_agent::bus::MessageKind::Chat));
    assert_eq!(chat.session_id, submitted.session_id);
    assert!(matches!(
        submitted.events.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn runtime_controls_reject_foreign_session_ownership_before_agent_access() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config.agents[0].access.allow_authenticated = true;
    let runtime = Runtime::boot_config(config).await.unwrap();
    let owner = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "owner",
            sylvander_protocol::AuthenticationMethod::PlatformIdentity,
        ),
        "channel-a",
        "test",
        "owner-request",
    );
    let session = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &owner,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "owned".into(),
            channel_id: Some("channel-a".into()),
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    let owner_context = sylvander_channel::UiService::context_report(
        runtime.ui_service.as_ref(),
        &owner,
        &session.session_id,
    )
    .await
    .expect("owner may inspect its context");
    assert_eq!(owner_context.model, "model-a");
    let attacker = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "attacker",
            sylvander_protocol::AuthenticationMethod::PlatformIdentity,
        ),
        "channel-a",
        "test",
        "attacker-request",
    );

    let context = sylvander_channel::UiService::context_report(
        runtime.ui_service.as_ref(),
        &attacker,
        &session.session_id,
    )
    .await
    .expect_err("foreign context inspection must be rejected");
    assert_eq!(
        context.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let compact = sylvander_channel::UiService::compact_session(
        runtime.ui_service.as_ref(),
        &attacker,
        &session.session_id,
    )
    .await
    .expect_err("foreign compaction must be rejected");
    assert_eq!(
        compact.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let preview = sylvander_channel::UiService::preview_workspace_rollback(
        runtime.ui_service.as_ref(),
        &attacker,
        &session.session_id,
    )
    .await
    .expect_err("foreign rollback preview must be rejected");
    assert_eq!(
        preview.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let rollback = sylvander_channel::UiService::rollback_workspace(
        runtime.ui_service.as_ref(),
        &attacker,
        &session.session_id,
        "turn-1",
    )
    .await
    .expect_err("foreign rollback must be rejected");
    assert_eq!(
        rollback.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );

    let deletion = sylvander_channel::UiService::delete_session(
        runtime.ui_service.as_ref(),
        &attacker,
        &session.session_id,
    )
    .await
    .expect_err("a foreign principal must not delete the session");
    assert_eq!(
        deletion.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    sylvander_channel::UiService::delete_session(
        runtime.ui_service.as_ref(),
        &owner,
        &session.session_id,
    )
    .await
    .expect("the owner may close the session through the Runtime lifecycle");
    assert!(
        runtime
            .session_store
            .get(&session.session_id)
            .await
            .unwrap()
            .is_none(),
        "Runtime deletion must remove the durable session"
    );
    assert!(
        runtime
            .engine
            .get_session(&session.session_id)
            .await
            .is_none(),
        "Runtime deletion must detach the active Agent session"
    );
    let guardian = runtime
        .guardian
        .as_ref()
        .expect("configured Runtime must start Guardian");
    wait_for_guardian_events(guardian, 2, 0).await;
    assert_eq!(
        guardian.canonical_record_count(),
        0,
        "session lifecycle references must not fabricate canonical memory"
    );

    runtime.shutdown().await.unwrap();
}

async fn attach_memory_session(
    runtime: &Runtime,
    agent: &str,
    user: &str,
) -> sylvander_agent::run::AuthenticatedSession {
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal {
            id: sylvander_protocol::PrincipalId::new(user),
            kind: sylvander_protocol::PrincipalKind::System,
            authentication: sylvander_protocol::AuthenticationMethod::UnixPeer,
            roles: Vec::new(),
        },
        "memory-test",
        "unix",
        format!("memory-test-{}", uuid::Uuid::new_v4()),
    );
    let created = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &boundary,
        SessionCreateRequest {
            agent_id: AgentId::new(agent),
            label: "memory-test".into(),
            channel_id: Some("memory-test".into()),
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    let stored = runtime
        .session_store
        .get(&created.session_id)
        .await
        .unwrap()
        .unwrap();
    let configured = runtime.configured_agent(&AgentId::new(agent)).unwrap();
    configured
        .attach_authenticated_session(created.session_id, stored.metadata)
        .await
        .unwrap()
}

#[test]
fn resolved_paths_default_and_preserve_memory_database() {
    let directory = tempfile::tempdir().unwrap();
    let data_dir = directory.path().join("data");
    let anchor_dir = directory.path().join("anchor");
    std::fs::create_dir_all(&anchor_dir).unwrap();
    let mut config = ServerConfig {
        schema_version: crate::config::CONFIG_SCHEMA_VERSION,
        server: crate::config::ServerSettings {
            data_dir: Some(data_dir.clone()),
            ..crate::config::ServerSettings::default()
        },
        model_providers: Vec::new(),
        execution_targets: Vec::new(),
        agents: Vec::new(),
        channels: Vec::new(),
    };
    config.server.memory_maintenance.integrity.backend = Some(MemoryIntegrityBackend::File {
        anchor_path: anchor_dir.join("state.json"),
    });

    let resolved = with_resolved_paths(config.clone()).unwrap();
    assert_eq!(resolved.server.memory_db, Some(data_dir.join("memory.db")));
    assert_eq!(
        resolved.server.user_profile_db,
        Some(data_dir.join("user-profiles.db"))
    );

    let explicit = directory.path().join("stores/custom-memory.db");
    config.server.memory_db = Some(explicit.clone());
    let resolved = with_resolved_paths(config).unwrap();
    assert_eq!(resolved.server.memory_db, Some(explicit));
}

#[tokio::test]
async fn configured_runtime_exposes_two_sided_identity_binding_end_to_end() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config.agents[0].access.allow_authenticated = true;
    let identity_key = directory.path().join("identity.key");
    std::fs::write(&identity_key, "abcdef0123456789abcdef0123456789").unwrap();
    config.server.identity.digest_key = Some(crate::config::SecretRef::File { path: identity_key });
    config.server.identity.trusted_issuers = vec![crate::config::IdentityIssuerSettings {
        transport: "unix".into(),
        channel_instance_id: "terminal".into(),
        principal_id: "local-alice".into(),
        user_id: "alice".into(),
    }];
    let runtime = Runtime::boot_config(config).await.unwrap();
    let context = ChannelContext::with_runtime_services(
        runtime.bus(),
        runtime.session_store.clone(),
        runtime.ui_service.clone(),
        None,
    );
    assert_eq!(
        context.identity_binding_capabilities(),
        IdentityBindingCapabilities::current()
    );

    let local = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "local-alice",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "terminal",
        "unix",
        "identity-begin",
    );
    let issued = context
        .submit_identity_binding(
            &local,
            IdentityBindingRequest {
                version: sylvander_protocol::IDENTITY_BINDING_PROTOCOL_VERSION,
                action: sylvander_protocol::IdentityBindingAction::Begin {},
            },
        )
        .await;
    let IdentityBindingResponse::ChallengeIssued {
        challenge_id,
        secret,
        ..
    } = issued
    else {
        panic!("configured identity service did not issue a challenge: {issued:?}");
    };

    let external = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "telegram-42",
            sylvander_protocol::AuthenticationMethod::PlatformIdentity,
        ),
        "bot-primary",
        "telegram",
        "identity-confirm",
    );
    let confirmed = context
        .submit_identity_binding(
            &external,
            IdentityBindingRequest {
                version: sylvander_protocol::IDENTITY_BINDING_PROTOCOL_VERSION,
                action: sylvander_protocol::IdentityBindingAction::Confirm {
                    challenge_id,
                    proof: secret.into_confirmation_proof(),
                },
            },
        )
        .await;
    assert!(matches!(
        confirmed,
        IdentityBindingResponse::Resolved { binding, .. }
            if binding.user_id == UserId::new("alice") && binding.revision == 1
    ));
    let profile_created = sylvander_channel::UiService::user_profile(
        runtime.ui_service.as_ref(),
        &local,
        UserProfileRequest {
            version: USER_PROFILE_PROTOCOL_VERSION,
            action: UserProfileAction::Create {
                profile: sylvander_protocol::UserProfileData::default(),
            },
        },
    )
    .await;
    assert!(matches!(
        profile_created,
        UserProfileResponse::Created { profile, .. } if profile.revision == 1
    ));
    let profile_from_external = sylvander_channel::UiService::user_profile(
        runtime.ui_service.as_ref(),
        &external,
        UserProfileRequest {
            version: USER_PROFILE_PROTOCOL_VERSION,
            action: UserProfileAction::Read {},
        },
    )
    .await;
    assert!(matches!(
        profile_from_external,
        UserProfileResponse::Read { profile, .. } if profile.revision == 1
    ));
    let profile_audits = runtime
        .ui_service
        .evidence
        .as_ref()
        .unwrap()
        .administration_audits(10)
        .await
        .unwrap();
    assert!(profile_audits.iter().any(|audit| {
        audit.operation == "user_profile_create"
            && audit.resource_kind == "user_profile"
            && audit.outcome == "succeeded"
    }));
    assert!(profile_audits.iter().any(|audit| {
        audit.operation == "user_profile_read"
            && audit.resource_kind == "user_profile"
            && audit.outcome == "succeeded"
    }));
    let created = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &local,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "stable user across channels".into(),
            channel_id: Some("terminal".into()),
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    let from_external = sylvander_channel::UiService::session_config(
        runtime.ui_service.as_ref(),
        &external,
        &created.session_id,
    )
    .await
    .expect("a linked external principal must resolve to the same stable user");
    assert_eq!(from_external.session_id, created.session_id);
    let stored = runtime
        .session_store
        .get(&created.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.metadata.user_id, "alice");
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn boot_spawns_agents() {
    let config = SystemConfig {
        name: "test-runtime".into(),
        agents: vec![test_spec("agent-1"), test_spec("agent-2")],
        sessions: vec![],
    };

    let rt = Runtime::boot(config, test_client()).await.expect("boot");
    assert_eq!(rt.engine.list_agents().await.len(), 2);
    rt.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn shutdown_cancels_owned_channel_tasks_before_returning() {
    let runtime = Runtime::boot(
        SystemConfig {
            name: "test-runtime".into(),
            agents: Vec::new(),
            sessions: Vec::new(),
        },
        test_client(),
    )
    .await
    .unwrap();
    let started = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    runtime
        .start_channels(vec![channel_registration(
            "blocking-1",
            BlockingChannel {
                started: started.clone(),
                dropped: dropped.clone(),
            },
        )])
        .await
        .unwrap();
    started.notified().await;

    runtime.shutdown().await.unwrap();
    assert!(dropped.load(Ordering::SeqCst));
}

#[tokio::test]
async fn channel_exit_before_readiness_fails_startup() {
    let runtime = Runtime::boot(
        SystemConfig {
            name: "test-runtime".into(),
            agents: Vec::new(),
            sessions: Vec::new(),
        },
        test_client(),
    )
    .await
    .unwrap();

    let error = runtime
        .start_channels(vec![channel_registration("exiting-1", ExitingChannel)])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("before becoming ready"));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn startup_failure_drains_channels_that_are_already_ready() {
    let runtime = Runtime::boot(
        SystemConfig {
            name: "test-runtime".into(),
            agents: Vec::new(),
            sessions: Vec::new(),
        },
        test_client(),
    )
    .await
    .unwrap();
    let dropped = Arc::new(AtomicBool::new(false));

    let error = runtime
        .start_channels(vec![
            channel_registration(
                "blocking-1",
                BlockingChannel {
                    started: Arc::new(Notify::new()),
                    dropped: dropped.clone(),
                },
            ),
            channel_registration("exiting-1", ExitingChannel),
        ])
        .await
        .unwrap_err();

    assert!(error.to_string().contains("before becoming ready"));
    assert!(dropped.load(Ordering::SeqCst));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn channel_exit_after_readiness_is_reported() {
    let runtime = Runtime::boot(
        SystemConfig {
            name: "test-runtime".into(),
            agents: Vec::new(),
            sessions: Vec::new(),
        },
        test_client(),
    )
    .await
    .unwrap();
    let exit = Arc::new(Notify::new());
    runtime
        .start_channels(vec![
            channel_registration("ready-exit-1", ReadyThenExitChannel { exit: exit.clone() })
                .with_restart_policy(ChannelRestartPolicy {
                    max_attempts: 0,
                    initial_backoff: Duration::ZERO,
                    max_backoff: Duration::ZERO,
                }),
        ])
        .await
        .unwrap();

    exit.notify_one();
    let channel = tokio::time::timeout(
        tokio::time::Duration::from_secs(1),
        runtime.wait_for_channel_exit(),
    )
    .await
    .unwrap();
    assert_eq!(channel.as_deref(), Some("ready-exit-1"));
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn ready_channel_is_restarted_and_health_is_instance_scoped() {
    let runtime = Runtime::boot(
        SystemConfig {
            name: "test-runtime".into(),
            agents: Vec::new(),
            sessions: Vec::new(),
        },
        test_client(),
    )
    .await
    .unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    runtime
        .start_channels(vec![
            channel_registration(
                "restart-1",
                RestartOnceChannel {
                    attempts: attempts.clone(),
                },
            )
            .with_restart_policy(ChannelRestartPolicy {
                max_attempts: 2,
                initial_backoff: Duration::from_millis(1),
                max_backoff: Duration::from_millis(1),
            }),
        ])
        .await
        .unwrap();

    tokio::time::timeout(Duration::from_secs(1), async {
        while attempts.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let health = runtime.channel_health().await;
    assert_eq!(health.len(), 1);
    assert_eq!(health[0].instance_id, "restart-1");
    assert_eq!(health[0].kind, "restart-once-test");
    assert_eq!(health[0].status, ChannelStatus::Ready);
    assert_eq!(health[0].restart_count, 1);
    let snapshot = runtime.operational_snapshot().await.unwrap();
    assert!(snapshot.ready);
    assert_eq!(snapshot.agent_count, 0);
    assert_eq!(snapshot.persistent_session_count, 0);
    assert!(snapshot.bus.bounded);
    assert_eq!(snapshot.bus.subscription_capacity, 256);
    assert_eq!(snapshot.channels, health);

    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn boot_loads_persistent_sessions() {
    let config = SystemConfig {
        name: "test-runtime".into(),
        agents: vec![test_spec("agent-1")],
        sessions: vec![StoredSession::new(
            SessionId::new("persistent-1"),
            "persistent-chat",
            SessionLifetime::Persistent,
            test_metadata(),
            vec![AgentId::new("agent-1")],
        )],
    };

    let rt = Runtime::boot(config, test_client()).await.expect("boot");
    assert_eq!(rt.engine.list_sessions().await.len(), 1);
    assert!(
        rt.engine
            .get_session(&SessionId::new("persistent-1"))
            .await
            .is_some()
    );
    rt.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn production_boot_validates_pins_from_a_qualified_versioned_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("qualified.db");
    let secret = directory.path().join("provider.key");
    std::fs::write(&secret, "0123456789abcdef0123456789abcdef").unwrap();
    let data_dir = directory.path().join("runtime-data");
    let anchor_dir = directory.path().join("integrity-anchor");
    std::fs::create_dir_all(&anchor_dir).unwrap();
    let input = format!(
        r#"
schema_version = 1
[server]
data_dir = "{}"
session_db = "{}"

[server.memory_maintenance.integrity]
[server.memory_maintenance.integrity.key]
source = "file"
path = "{}"
[server.memory_maintenance.integrity.backend]
kind = "file"
anchor_path = "{}"

[[model_providers]]
id = "alpha"
base_url = "https://alpha.invalid"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "shared"

[[model_providers]]
id = "beta"
base_url = "https://beta.invalid"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "shared"

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
allowed_models = [
  {{ provider_id = "alpha", model_id = "shared" }},
  {{ provider_id = "beta", model_id = "shared" }},
]
"#,
        data_dir.display(),
        database.display(),
        secret.display(),
        anchor_dir.join("anchor.json").display(),
        secret.display(),
        secret.display()
    );
    let mut config = ServerConfig::from_toml(&input).unwrap();
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
    let runtime = Runtime::boot_config(config).await.unwrap();
    let agent = runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap();
    let effective = resolve_session_config(
        agent,
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
    let mut session = StoredSession::new(
        SessionId::new("qualified-pins"),
        "qualified pins",
        SessionLifetime::Persistent,
        test_metadata(),
        vec![AgentId::new("assistant")],
    );
    session.effective_config = Some(effective);

    let closed = close_session_revision_pins(
        runtime.ui_service.agent_registry.as_ref().unwrap(),
        &session,
        agent,
    )
    .await
    .unwrap();

    assert!(!closed.changed);
    assert_eq!(closed.effective.provider_id, "beta");
    let pins = closed.effective.require_revision_pins().unwrap();
    assert_eq!(pins.provider_revision, 1);
    assert_eq!(pins.model_revision, 1);
}

#[tokio::test]
async fn production_boot_rejects_old_and_unknown_memory_schemas_without_fallback() {
    for version in [1_i64, 999_i64] {
        let directory = tempfile::tempdir().unwrap();
        let memory_db = directory.path().join("memory.db");
        let connection = rusqlite::Connection::open(&memory_db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE memory_schema_migrations (\
                     component TEXT PRIMARY KEY, version INTEGER NOT NULL);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO memory_schema_migrations(component, version) \
                     VALUES ('relationship_memory', ?1)",
                [version],
            )
            .unwrap();
        drop(connection);
        let mut config = configured_memory_test_config(&directory, &["assistant"]);
        config.server.memory_db = Some(memory_db.clone());

        let error = match Runtime::boot_config(config).await {
            Ok(runtime) => {
                runtime.shutdown().await.unwrap();
                panic!("unsupported memory schema must fail production boot")
            }
            Err(error) => error,
        };
        assert!(matches!(error, RuntimeError::Store(_)));
        assert_eq!(
            error.to_string(),
            "store error: store error: unsupported relationship memory schema"
        );
        assert!(!error.to_string().contains(&memory_db.display().to_string()));
    }
}

#[tokio::test]
async fn production_boot_requires_anchor_outside_data_directory() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    let data_dir = config.server.data_dir.clone().unwrap();
    std::fs::create_dir_all(data_dir.join("anchor")).unwrap();
    config.server.memory_maintenance.integrity.backend = Some(MemoryIntegrityBackend::File {
        anchor_path: data_dir.join("anchor/state.json"),
    });

    let error = match Runtime::boot_config(config).await {
        Ok(runtime) => {
            runtime.shutdown().await.unwrap();
            panic!("anchor within data directory must fail production boot")
        }
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "configuration error: memory integrity anchor must be outside the runtime data directory"
    );
}

#[tokio::test]
async fn production_restart_rejects_database_writer_tampering() {
    let directory = tempfile::tempdir().unwrap();
    let config = configured_memory_test_config(&directory, &["assistant"]);
    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    let session = attach_memory_session(&runtime, "assistant", "alice").await;
    runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap()
        .run
        .remember_entry(&session, MemoryAppend::new("trusted"))
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();

    let memory_db = config.server.data_dir.as_ref().unwrap().join("memory.db");
    let connection = rusqlite::Connection::open(memory_db).unwrap();
    connection
        .execute("UPDATE relationship_memories SET content = 'forged'", [])
        .unwrap();
    drop(connection);
    let error = match Runtime::boot_config(config).await {
        Ok(runtime) => {
            runtime.shutdown().await.unwrap();
            panic!("tampered memory database must fail production restart")
        }
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "store error: store error: memory integrity verification failed"
    );
}

#[tokio::test]
async fn production_memory_isolates_same_user_across_agent_owners() {
    let directory = tempfile::tempdir().unwrap();
    let config = configured_memory_test_config(&directory, &["agent-a", "agent-b"]);
    let runtime = Runtime::boot_config(config).await.unwrap();
    let session_a = attach_memory_session(&runtime, "agent-a", "same-user").await;
    let session_b = attach_memory_session(&runtime, "agent-b", "same-user").await;
    let agent_a = &runtime
        .configured_agent(&AgentId::new("agent-a"))
        .unwrap()
        .run;
    let agent_b = &runtime
        .configured_agent(&AgentId::new("agent-b"))
        .unwrap()
        .run;

    let entry_a = agent_a
        .remember_entry(&session_a, MemoryAppend::new("agent A only"))
        .await
        .unwrap();
    let entry_b = agent_b
        .remember_entry(&session_b, MemoryAppend::new("agent B only"))
        .await
        .unwrap();

    assert!(
        runtime
            .configured_agent(&AgentId::new("agent-a"))
            .unwrap()
            .uses_memory_store(&runtime.memory_store)
    );
    assert!(
        runtime
            .configured_agent(&AgentId::new("agent-b"))
            .unwrap()
            .uses_memory_store(&runtime.memory_store)
    );
    assert_eq!(
        agent_a
            .recall(&session_a, "agent A only", MemoryFilter::default())
            .await
            .unwrap()
            .first()
            .unwrap()
            .content,
        "agent A only"
    );
    assert_eq!(
        agent_b
            .recall(&session_b, "agent B only", MemoryFilter::default())
            .await
            .unwrap()
            .first()
            .unwrap()
            .content,
        "agent B only"
    );
    assert!(
        agent_a
            .recall(&session_a, "agent B only", MemoryFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        agent_b
            .recall(&session_b, "agent A only", MemoryFilter::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert_ne!(entry_a.owner, entry_b.owner);
    runtime.shutdown().await.unwrap();
}

#[tokio::test]
async fn production_memory_preserves_revision_provenance_and_expiry_across_restart() {
    let directory = tempfile::tempdir().unwrap();
    let config = configured_memory_test_config(&directory, &["assistant"]);
    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    let session_id = attach_memory_session(&runtime, "assistant", "user-a").await;
    let entry = runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap()
        .run
        .remember_entry(
            &session_id,
            MemoryAppend::new("restart field fidelity").with_ttl(3600),
        )
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();
    drop(runtime);

    let restarted = Runtime::boot_config(config).await.unwrap();
    let restarted_session = attach_memory_session(&restarted, "assistant", "user-a").await;
    let restored = restarted
        .configured_agent(&AgentId::new("assistant"))
        .unwrap()
        .run
        .recall(
            &restarted_session,
            "restart field fidelity",
            MemoryFilter::default(),
        )
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(restored.revision, 1);
    assert_eq!(restored.revision, entry.revision);
    assert_eq!(restored.expires_at, entry.expires_at);
    assert!(restored.expires_at.is_some());
    assert_eq!(restored.provenance.actor, MemoryActorKind::Worker);
    assert!(
        restored
            .provenance
            .user_id
            .as_ref()
            .unwrap()
            .0
            .starts_with("unlinked:v1:")
    );
    assert_eq!(
        restored.provenance.agent_id.as_ref().unwrap().0,
        "assistant"
    );
    assert_eq!(restored.provenance.session_id, entry.provenance.session_id);
    assert_eq!(restored.provenance.trace_id, None);
    assert_eq!(restored.provenance.source, MemoryProvenanceSource::Runtime);
    assert!(restored.provenance.trusted);
    assert_eq!(restored.provenance, entry.provenance);
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn production_memory_catch_up_is_bounded_restart_safe_and_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config.server.memory_maintenance.batch_size = 1;
    config.server.memory_maintenance.max_batches_per_run = 2;
    config
        .server
        .memory_maintenance
        .retention
        .expired_grace_days = 0;
    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    assert!(runtime.memory_maintenance.is_some());
    let session = attach_memory_session(&runtime, "assistant", "user").await;
    let run = &runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap()
        .run;
    for content in ["one", "two", "three"] {
        run.remember_entry(&session, MemoryAppend::new(content).with_ttl(1))
            .await
            .unwrap();
    }
    runtime.shutdown().await.unwrap();
    assert!(
        runtime
            .memory_maintenance
            .as_ref()
            .unwrap()
            .is_stopped()
            .await
    );
    runtime.shutdown().await.unwrap();
    drop(runtime);

    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let memory_db = config.server.data_dir.as_ref().unwrap().join("memory.db");

    let restarted = Runtime::boot_config(config.clone()).await.unwrap();
    restarted.shutdown().await.unwrap();
    drop(restarted);
    let counts = || {
        let connection = rusqlite::Connection::open(&memory_db).unwrap();
        connection
                .query_row(
                    "SELECT (SELECT COUNT(*) FROM relationship_memories), (SELECT COUNT(*) FROM relationship_memory_audit WHERE operation = 'purge_expired')",
                    [],
                    |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
                )
                .unwrap()
    };
    assert_eq!(counts(), (1, 2));

    let restarted = Runtime::boot_config(config.clone()).await.unwrap();
    restarted.shutdown().await.unwrap();
    drop(restarted);
    assert_eq!(counts(), (0, 3));
    let restarted = Runtime::boot_config(config).await.unwrap();
    restarted.shutdown().await.unwrap();
    drop(restarted);
    assert_eq!(counts(), (0, 3));
}

#[tokio::test]
async fn startup_failure_leaves_policy_staged_and_previous_revision_restartable() {
    let directory = tempfile::tempdir().unwrap();
    let config = configured_memory_test_config(&directory, &["assistant"]);
    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    runtime.shutdown().await.unwrap();
    drop(runtime);

    let mut failed_rollout = config.clone();
    failed_rollout.server.memory_maintenance.retention.revision = 2;
    let invalid_evidence_path = directory.path().join("evidence-is-a-directory");
    std::fs::create_dir_all(&invalid_evidence_path).unwrap();
    failed_rollout.server.evidence.path = Some(invalid_evidence_path);
    let error = match Runtime::boot_config(failed_rollout).await {
        Ok(runtime) => {
            runtime.shutdown().await.unwrap();
            panic!("failed rollout must not complete startup")
        }
        Err(error) => error,
    };
    assert!(matches!(error, RuntimeError::Evidence(_)));

    let memory_db = config.server.data_dir.as_ref().unwrap().join("memory.db");
    let revisions = || {
        rusqlite::Connection::open(&memory_db)
                .unwrap()
                .query_row(
                    "SELECT (SELECT policy_revision FROM relationship_memory_retention_state WHERE singleton = 1), (SELECT policy_revision FROM relationship_memory_retention_policy_stage WHERE singleton = 1)",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap()
    };
    assert_eq!(revisions(), (1, 2));

    let previous = Runtime::boot_config(config.clone()).await.unwrap();
    previous.shutdown().await.unwrap();
    drop(previous);

    let mut retry = config;
    retry.server.memory_maintenance.retention.revision = 2;
    let activated = Runtime::boot_config(retry).await.unwrap();
    activated.shutdown().await.unwrap();
    drop(activated);
    let connection = rusqlite::Connection::open(memory_db).unwrap();
    assert_eq!(
            connection
                .query_row(
                    "SELECT policy_revision FROM relationship_memory_retention_state WHERE singleton = 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM relationship_memory_retention_policy_stage",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn maintenance_failure_keeps_the_concrete_durable_store_content_safely() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config
        .server
        .memory_maintenance
        .retention
        .expired_grace_days = 0;
    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    let session = attach_memory_session(&runtime, "assistant", "user").await;
    runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap()
        .run
        .remember_entry(&session, MemoryAppend::new("must remain durable"))
        .await
        .unwrap();
    runtime.shutdown().await.unwrap();
    drop(runtime);
    let memory_db = config.server.data_dir.as_ref().unwrap().join("memory.db");
    let policy =
        RuntimeMemoryMaintenancePolicy::from_settings(&config.server.memory_maintenance).unwrap();
    let store = SqliteMemoryStore::open_with_retention_policy(&memory_db, policy.retention.clone())
        .unwrap();
    let connection = rusqlite::Connection::open(&memory_db).unwrap();
    connection
        .execute_batch(
            "UPDATE relationship_memories SET expires_at = unixepoch() - 1; \
                 CREATE TRIGGER reject_runtime_purge BEFORE INSERT ON relationship_memory_audit \
                 WHEN NEW.operation LIKE 'purge_%' BEGIN SELECT RAISE(ABORT, 'private'); END;",
        )
        .unwrap();
    drop(connection);

    let error = memory_maintenance_catch_up(&store.maintenance(), &policy)
        .await
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "store error: memory retention catch-up failed"
    );
    assert!(!error.to_string().contains("private"));
    let count: u32 = rusqlite::Connection::open(memory_db)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM relationship_memories", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn periodic_memory_maintenance_runs_and_shutdown_joins_the_single_worker() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = configured_memory_test_config(&directory, &["assistant"]);
    config
        .server
        .memory_maintenance
        .retention
        .expired_grace_days = 0;
    config.server.memory_maintenance.batch_size = 1;
    config.server.memory_maintenance.max_batches_per_run = 100;
    let memory_db = directory.path().join("periodic-memory.db");
    config.server.memory_db = Some(memory_db.clone());
    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    let session = attach_memory_session(&runtime, "assistant", "user").await;
    let run = &runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap()
        .run;
    for index in 0..25 {
        run.remember_entry(&session, MemoryAppend::new(format!("periodic-{index}")))
            .await
            .unwrap();
    }
    runtime.shutdown().await.unwrap();
    drop(runtime);

    let policy = RuntimeMemoryMaintenancePolicy::from_settings(&config.server.memory_maintenance)
        .unwrap()
        .with_interval(std::time::Duration::from_millis(10));
    let store = SqliteMemoryStore::open_with_retention_policy(&memory_db, policy.retention.clone())
        .unwrap();
    let maintenance =
        MemoryMaintenanceTask::start(store.maintenance(), policy, directory.path().into());
    rusqlite::Connection::open(&memory_db)
        .unwrap()
        .execute(
            "UPDATE relationship_memories SET expires_at = unixepoch() - 1",
            [],
        )
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let count: u32 = rusqlite::Connection::open(&memory_db)
                .unwrap()
                .query_row("SELECT COUNT(*) FROM relationship_memories", [], |row| {
                    row.get(0)
                })
                .unwrap();
            if count < 25 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    maintenance.shutdown().await;
    assert!(maintenance.is_stopped().await);
    let remaining: u32 = rusqlite::Connection::open(&memory_db)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM relationship_memories", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!((1..25).contains(&remaining));
    maintenance.shutdown().await;
}

#[tokio::test]
async fn configured_memory_is_shared_across_recomposition_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    let secret = directory.path().join("provider.key");
    std::fs::write(&secret, "test-secret").unwrap();
    let mut config = ServerConfig::from_toml(&format!(
        r#"
schema_version = 1
[server]
data_dir = "{}"

[[model_providers]]
id = "primary"
base_url = "https://models.invalid"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "model-a"

[[agents]]
[agents.spec]
id = "assistant"
name = "Sylvander"
[agents.spec.model]
provider = "primary"
model_name = "model-a"
allowed_models = [{{ provider_id = "primary", model_id = "model-a" }}]
"#,
        directory.path().display(),
        secret.display()
    ))
    .unwrap();
    configure_test_memory_integrity(&mut config, directory.path(), &secret);
    let runtime = Runtime::boot_config(config.clone()).await.unwrap();
    let provider = runtime.revision_provider.as_ref().unwrap();
    assert!(Arc::ptr_eq(&runtime.memory_store, &provider.memory));
    assert!(
        runtime
            .configured_agent(&AgentId::new("assistant"))
            .unwrap()
            .uses_memory_store(&runtime.memory_store)
    );
    let session_id = attach_memory_session(&runtime, "assistant", "user-a").await;
    runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap()
        .run
        .remember_entry(&session_id, MemoryAppend::new("durable shared memory"))
        .await
        .unwrap();
    provider.configured.write().await.clear();
    assert!(
        provider
            .configured_revision(&AgentId::new("assistant"), 1)
            .await
            .unwrap()
            .uses_memory_store(&runtime.memory_store)
    );
    assert!(
        provider
            .revalidate_revision(&AgentId::new("assistant"), 1)
            .await
            .unwrap()
            .uses_memory_store(&runtime.memory_store)
    );
    runtime.shutdown().await.unwrap();
    drop(runtime);

    let restarted = Runtime::boot_config(config).await.unwrap();
    let restarted_session = attach_memory_session(&restarted, "assistant", "user-a").await;
    assert_eq!(
        restarted
            .configured_agent(&AgentId::new("assistant"))
            .unwrap()
            .run
            .recall(
                &restarted_session,
                "durable shared memory",
                MemoryFilter::default(),
            )
            .await
            .unwrap()
            .first()
            .unwrap()
            .content,
        "durable shared memory"
    );
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn configured_boot_restores_database_session_after_agent_spawn() {
    let model_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_revision_probe",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "configured revision"}],
            "model": "model-a",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 4, "output_tokens": 2}
        })))
        .mount(&model_server)
        .await;
    let directory = tempfile::TempDir::new().unwrap();
    let database = directory.path().join("sessions.db");
    let secret = directory.path().join("provider.key");
    std::fs::write(&secret, "test-secret").unwrap();
    let input = format!(
        r#"
schema_version = 1

[server]
data_dir = "{}"
session_db = "{}"

[[model_providers]]
id = "primary"
base_url = "{}"

[model_providers.api_key]
source = "file"
path = "{}"

[[model_providers.models]]
id = "model-a"
capabilities = ["tool_use"]

[[model_providers.models]]
id = "model-b"
capabilities = ["tool_use"]

[[agents]]
allow_session_prompt = false

[agents.access]
allowed_principals = ["test-user", "telegram:bot-a:42"]

[agents.spec]
id = "assistant"
name = "Sylvander"

[agents.spec.model]
provider = "primary"
model_name = "model-a"
allowed_models = [{{ provider_id = "primary", model_id = "model-a" }}]
"#,
        directory.path().display(),
        database.display(),
        model_server.uri(),
        secret.display()
    );
    let mut config = ServerConfig::from_toml(&input).unwrap();
    configure_test_memory_integrity(&mut config, directory.path(), &secret);
    let explicit_definition = config.agents[0].clone();
    let explicit_selection = active_snapshot_selection(&explicit_definition);
    assert_eq!(
        explicit_selection.allowed_models,
        BTreeSet::from([ModelSelection {
            provider_id: "primary".into(),
            model_id: "model-a".into(),
        }])
    );
    config.agents[0].spec.persona.system_prompt = "revision one prompt".into();
    let restart_config = config.clone();
    let first_runtime = Runtime::boot_config(config).await.unwrap();
    let agent = first_runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap();
    let mut stored = StoredSession::new(
        SessionId::new("restored-session"),
        "restored",
        SessionLifetime::Persistent,
        test_metadata(),
        vec![AgentId::new("assistant")],
    );
    stored.config_overrides.user_workspace = Some(sylvander_protocol::SessionWorkspaceBinding {
        execution_target: "local".into(),
        path: stored.metadata.workspace.clone(),
        read_only: false,
        instruction_focus: None,
    });
    stored.effective_config =
        Some(resolve_session_config(agent, &stored.config_overrides, None, None).unwrap());
    first_runtime.session_store.save(&stored).await.unwrap();
    first_runtime.shutdown().await.unwrap();
    let runtime = Runtime::boot_config(restart_config.clone()).await.unwrap();

    assert!(
        runtime
            .engine
            .get_session(&SessionId::new("restored-session"))
            .await
            .is_some()
    );
    assert!(
        runtime
            .configured_agent(&AgentId::new("assistant"))
            .is_some()
    );
    let restored = runtime
        .session_store
        .get(&SessionId::new("restored-session"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.config_revision, 0);
    let effective = restored.effective_config.unwrap();
    assert_eq!(effective.agent_id, AgentId::new("assistant"));
    assert_eq!(effective.model_id, "model-a");
    let pins = effective.require_revision_pins().unwrap();
    assert_eq!(pins.provider_revision, 1);
    assert_eq!(pins.model_revision, 1);
    assert_eq!(effective.execution_target, "local");
    assert_eq!(
        effective.provenance.user_workspace.kind,
        sylvander_protocol::SessionConfigSourceKind::SessionOverride
    );
    let registry = runtime.ui_service.agent_registry.as_ref().unwrap();
    let active_agent = runtime
        .configured_agent(&AgentId::new("assistant"))
        .unwrap();
    let mut unconfigured = StoredSession::new(
        SessionId::new("unconfigured-pin-probe"),
        "unconfigured pin probe",
        SessionLifetime::Persistent,
        test_metadata(),
        vec![AgentId::new("assistant")],
    );
    assert!(matches!(
        close_session_revision_pins(registry, &unconfigured, active_agent).await,
        Err(SessionBindingError::UnresolvedPins(_))
    ));
    runtime.session_store.save(&unconfigured).await.unwrap();
    assert!(
        runtime
            .revision_provider
            .as_ref()
            .unwrap()
            .revision_for_session(&AgentId::new("assistant"), &unconfigured.id)
            .await
            .is_err(),
        "execution routing must not repair unresolved pins on demand"
    );
    runtime
        .session_store
        .delete(&unconfigured.id)
        .await
        .unwrap();

    unconfigured.effective_config = Some(effective.clone());
    let already_closed = close_session_revision_pins(registry, &unconfigured, active_agent)
        .await
        .unwrap();
    assert!(!already_closed.changed);

    let mut mismatched = unconfigured;
    let mut invalid = effective.clone();
    invalid.model_revision = 99;
    mismatched.effective_config = Some(invalid);
    assert!(matches!(
        close_session_revision_pins(registry, &mismatched, active_agent).await,
        Err(SessionBindingError::ModelRevisionMismatch {
            expected: 1,
            actual: 99
        })
    ));
    let (revision, updated) = runtime
        .update_session_config(
            &SessionId::new("restored-session"),
            0,
            SessionConfigOverrides {
                model: Some(ModelSelection {
                    provider_id: "primary".into(),
                    model_id: "model-a".into(),
                }),
                ..restored.config_overrides.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(revision, 1);
    assert_eq!(
        updated.provenance.model.kind,
        sylvander_protocol::SessionConfigSourceKind::SessionOverride
    );
    assert!(
        runtime
            .update_session_config(
                &SessionId::new("restored-session"),
                0,
                SessionConfigOverrides::default(),
            )
            .await
            .is_err(),
        "a stale client must not overwrite a newer configuration"
    );
    let owner = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "test-user",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "tui-local",
        "unix",
        "request-create",
    );
    let before_invalid_create = runtime.session_store.list_persistent().await.unwrap().len();
    let invalid_create = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &owner,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "invalid prompt must not persist".into(),
            channel_id: Some("tui-local".into()),
            overrides: SessionConfigOverrides {
                system_prompt: Some(String::new()),
                ..SessionConfigOverrides::default()
            },
        },
    )
    .await
    .unwrap_err();
    assert!(
        invalid_create
            .message
            .contains("prompt configuration is invalid")
    );
    assert_eq!(
        runtime.session_store.list_persistent().await.unwrap().len(),
        before_invalid_create,
        "invalid session prompt must fail before session persistence"
    );
    let created = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &owner,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "created through UI service".into(),
            channel_id: Some("tui-local".into()),
            overrides: SessionConfigOverrides {
                model: Some(ModelSelection {
                    provider_id: "primary".into(),
                    model_id: "model-a".into(),
                }),
                ..SessionConfigOverrides::default()
            },
        },
    )
    .await
    .unwrap();
    assert!(created.effective.require_revision_pins().is_ok());
    let stored = runtime
        .session_store
        .get(&created.session_id)
        .await
        .unwrap()
        .expect("created session must be durable");
    assert_eq!(stored.effective_config, Some(created.effective));
    assert!(stored.metadata.user_id.starts_with("unlinked:v1:"));
    assert_eq!(stored.external_meta["channel_id"], "tui-local");
    let invalid_update = sylvander_channel::UiService::update_session_config(
        runtime.ui_service.as_ref(),
        &owner,
        SessionConfigUpdateRequest {
            session_id: created.session_id.clone(),
            expected_revision: created.revision,
            overrides: SessionConfigOverrides {
                system_prompt: Some("private\0prompt".into()),
                ..SessionConfigOverrides::default()
            },
        },
    )
    .await
    .unwrap_err();
    assert!(
        invalid_update
            .message
            .contains("prompt configuration is invalid")
    );
    assert!(!invalid_update.message.contains("private"));
    let unchanged = runtime
        .session_store
        .get(&created.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.config_revision, created.revision);
    assert!(unchanged.config_overrides.system_prompt.is_none());
    assert!(
        runtime
            .revision_provider
            .as_ref()
            .unwrap()
            .revision_for_session(&AgentId::new("different-agent"), &created.session_id)
            .await
            .is_err(),
        "a session revision binding must never be reused for another Agent"
    );
    let peer = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &owner,
        SessionCreateRequest {
            agent_id: AgentId::new("assistant"),
            label: "unmodified peer session".into(),
            channel_id: Some("tui-local".into()),
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    let restricted = sylvander_protocol::PermissionProfile {
        file_access: sylvander_protocol::FileAccess::ReadOnly,
        network_access: sylvander_protocol::NetworkAccess::Denied,
        approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
    };
    let selected = sylvander_channel::UiService::update_session_config(
        runtime.ui_service.as_ref(),
        &owner,
        SessionConfigUpdateRequest {
            session_id: created.session_id.clone(),
            expected_revision: created.revision,
            overrides: SessionConfigOverrides {
                model: Some(ModelSelection {
                    provider_id: "primary".into(),
                    model_id: "model-a".into(),
                }),
                permissions: Some(restricted.clone()),
                ..SessionConfigOverrides::default()
            },
        },
    )
    .await
    .unwrap();
    assert_eq!(selected.effective.permissions, restricted);
    let peer_after = sylvander_channel::UiService::session_config(
        runtime.ui_service.as_ref(),
        &owner,
        &peer.session_id,
    )
    .await
    .unwrap();
    assert_eq!(
        peer_after, peer,
        "one session override must not leak to another"
    );
    let missing_session = sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &owner,
        &sylvander_protocol::UiClientMessage::SelectModel {
            session_id: None,
            model: ModelSelection {
                provider_id: "primary".into(),
                model_id: "model-a".into(),
            },
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
        },
    )
    .await
    .expect_err("selection without session identity must fail closed");
    assert_eq!(
        missing_session.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let other_terminal = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "test-user",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "other-terminal",
        "unix",
        "request-cross-instance",
    );
    let denial = sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &other_terminal,
        &sylvander_protocol::UiClientMessage::GetSessionConfig {
            session_id: created.session_id.0.clone(),
        },
    )
    .await
    .expect_err("the same principal from another channel instance must be denied");
    assert_eq!(
        denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let platform_boundary = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "telegram:bot-a:42",
            sylvander_protocol::AuthenticationMethod::PlatformIdentity,
        ),
        "bot-a",
        "telegram",
        "telegram-update-1",
    );
    let channel_context = ChannelContext::with_runtime_services(
        runtime.bus(),
        runtime.session_store.clone(),
        runtime.ui_service.clone(),
        None,
    );
    let mut platform_submission = sylvander_channel::submit_external_chat(
        &channel_context,
        &platform_boundary,
        sylvander_channel::ExternalChatRequest {
            existing_session: None,
            agent_id: AgentId::new("assistant"),
            label: "telegram-42".into(),
            overrides: SessionConfigOverrides::default(),
            text: "hello from Telegram".into(),
            attachments: Vec::new(),
            external_meta: std::collections::BTreeMap::from([
                ("channel_instance_id".into(), "bot-a".into()),
                ("chat_id".into(), "42".into()),
            ]),
        },
    )
    .await
    .expect("an allowed platform principal may create and use its session");
    let platform_session = platform_submission.session_id.clone();
    let routed = platform_submission
        .events
        .recv()
        .await
        .expect("the authenticated user message must be routed");
    assert_eq!(routed.session_id, platform_session);
    assert_eq!(
        routed.recipient,
        sylvander_agent::bus::Recipient::Agent(AgentId::new("assistant"))
    );
    let platform_stored = runtime
        .session_store
        .get(&platform_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        routed.sender,
        sylvander_agent::bus::Sender::User(platform_stored.metadata.user_id.clone())
    );
    assert!(platform_stored.metadata.user_id.starts_with("unlinked:v1:"));
    assert_eq!(
        platform_stored.external_meta["channel_instance_id"],
        "bot-a"
    );
    assert!(platform_stored.effective_config.is_some());
    let other_bot = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "telegram:bot-b:42",
            sylvander_protocol::AuthenticationMethod::PlatformIdentity,
        ),
        "bot-b",
        "telegram",
        "telegram-update-2",
    );
    let mut victim_inbox = runtime
        .bus()
        .subscribe(SubscriptionFilter {
            session_ids: Some(vec![platform_session.clone()]),
            recipients: Some(vec![Recipient::Agent(AgentId::new("assistant"))]),
            kinds: None,
        })
        .await
        .unwrap();
    let control_denial = channel_context
        .submit_control(
            &other_bot,
            sylvander_protocol::UiClientMessage::Approve {
                session_id: platform_session.0.clone(),
                call_id: "victim-call".into(),
                approved: true,
                scope: sylvander_protocol::ApprovalScope::Once,
                reason: None,
            },
        )
        .await
        .expect_err("an external channel must not control a victim session");
    assert_eq!(
        control_denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    assert!(matches!(
        victim_inbox.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    let denial = sylvander_channel::submit_external_chat(
        &channel_context,
        &other_bot,
        sylvander_channel::ExternalChatRequest {
            existing_session: Some(platform_session),
            agent_id: AgentId::new("assistant"),
            label: "telegram-42".into(),
            overrides: SessionConfigOverrides::default(),
            text: "cross-instance attempt".into(),
            attachments: Vec::new(),
            external_meta: std::collections::BTreeMap::new(),
        },
    )
    .await
    .expect_err("another channel instance must not reuse the session");
    assert_eq!(
        denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let stranger = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "other-user",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "tui-local",
        "unix",
        "request-read",
    );
    assert!(
        sylvander_channel::UiService::discover_agents(runtime.ui_service.as_ref(), &stranger,)
            .await
            .unwrap()
            .is_empty()
    );
    let denial = sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &stranger,
        &sylvander_protocol::UiClientMessage::CreateSession {
            request: SessionCreateRequest {
                agent_id: AgentId::new("assistant"),
                label: "unauthorized".into(),
                channel_id: Some("tui-local".into()),
                overrides: SessionConfigOverrides::default(),
            },
        },
    )
    .await
    .expect_err("an Agent allowlist must be enforced before creation");
    assert_eq!(
        denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let denial = sylvander_channel::UiService::session_config(
        runtime.ui_service.as_ref(),
        &stranger,
        &created.session_id,
    )
    .await
    .expect_err("a different principal must not read the session");
    assert_eq!(
        denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let chat_denial = sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &stranger,
        &sylvander_protocol::UiClientMessage::Chat {
            text: "cross-session attempt".into(),
            attachments: Vec::new(),
            session_id: Some(created.session_id.0.clone()),
            workspace: None,
        },
    )
    .await
    .expect_err("message dispatch must enforce the same ownership boundary");
    assert_eq!(
        chat_denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let unauthenticated = sylvander_protocol::BoundaryContext::unauthenticated(
        "websocket",
        "websocket",
        "request-ping",
    );
    let denial = sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &unauthenticated,
        &sylvander_protocol::UiClientMessage::Ping,
    )
    .await
    .expect_err("an unauthenticated transport must fail closed");
    assert_eq!(
        denial.code,
        sylvander_protocol::BoundaryErrorCode::Unauthenticated
    );
    let authentication_boundary = sylvander_protocol::BoundaryContext::unauthenticated(
        "websocket",
        "websocket",
        "request-authentication-failure",
    );
    let authentication_denial = sylvander_channel::UiService::reject_authentication(
        runtime.ui_service.as_ref(),
        &authentication_boundary,
        sylvander_protocol::AuthenticationFailure::new(
            sylvander_protocol::AuthenticationMethod::BearerToken,
        ),
    )
    .await;
    assert_eq!(
        authentication_denial.code,
        sylvander_protocol::BoundaryErrorCode::Unauthenticated
    );
    assert!(
        runtime
            .engine
            .get_session(&created.session_id)
            .await
            .is_some()
    );
    let evidence = runtime
        .evidence_store()
        .expect("evidence enabled by default");
    evidence
        .start_run("feedback-auth-run".into(), "test".into(), 10)
        .await
        .unwrap();
    evidence
        .start_turn(crate::evidence::TurnStart {
            id: "feedback-auth-turn".into(),
            run_id: "feedback-auth-run".into(),
            session_id: created.session_id.0.clone(),
            agent_id: Some("assistant".into()),
            started_at: 11,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .unwrap();
    let feedback = RunFeedback {
        target: crate::evidence::feedback_target("feedback-auth-run", "feedback-auth-turn"),
        rating: sylvander_protocol::FeedbackRating::Positive,
        note: None,
        correction: Some("prefer the verified result".into()),
        tags: Vec::new(),
        task_result: Some(sylvander_protocol::FeedbackTaskResult::Succeeded),
        artifacts: Vec::new(),
        validations: vec![sylvander_protocol::EvidenceReference {
            locator: "test:runtime-controls".into(),
            digest_sha256: Some("a".repeat(64)),
        }],
        privacy_class: sylvander_protocol::FeedbackPrivacyClass::Private,
    };
    let feedback_message = sylvander_protocol::UiClientMessage::SubmitFeedback {
        feedback: feedback.clone(),
    };
    let feedback_id = sylvander_channel::UiService::submit_feedback(
        runtime.ui_service.as_ref(),
        &owner,
        feedback,
    )
    .await
    .expect("the session owner may submit feedback");
    let stored_feedback = evidence
        .feedback(feedback_id)
        .await
        .unwrap()
        .expect("submitted feedback must be readable from the evidence ledger");
    assert_eq!(
        stored_feedback.attribution.principal_digest,
        sha256_text("test-user")
    );
    assert_eq!(stored_feedback.attribution.channel_instance_id, "tui-local");
    assert_eq!(stored_feedback.attribution.transport, "unix");
    assert_eq!(
        stored_feedback.correction.as_deref(),
        Some("prefer the verified result")
    );
    assert_eq!(
        stored_feedback.task_result,
        Some(sylvander_protocol::FeedbackTaskResult::Succeeded)
    );
    let guardian = runtime
        .guardian
        .as_ref()
        .expect("configured Runtime must start Guardian");
    wait_for_guardian_events(guardian, 1, 0).await;
    assert_eq!(guardian.canonical_record_count(), 0);
    let denial = sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &stranger,
        &feedback_message,
    )
    .await
    .expect_err("another principal must not submit feedback for the turn");
    assert_eq!(
        denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    evidence
        .finish_run("feedback-auth-run".into(), 12, "succeeded")
        .await
        .unwrap();
    let denials = evidence.authorization_denials(10).await.unwrap();
    assert_eq!(denials.len(), 9);
    let authentication_audit = denials
        .iter()
        .find(|denial| denial.operation == "authenticate_bearer_token")
        .expect("authentication rejection must be audited by the runtime");
    assert!(authentication_audit.principal_digest.is_none());
    assert!(authentication_audit.resource_digest.is_none());
    assert!(
        denials
            .iter()
            .all(|denial| denial.principal_digest.is_some() || denial.code == "unauthenticated")
    );
    assert!(
        denials
            .iter()
            .all(|denial| denial.resource_digest.as_deref() != Some(created.session_id.0.as_str()))
    );
    let original_revision = restart_config.agents[0].revision;
    let mut next_definition = restart_config.agents[0].clone();
    next_definition.revision += 1;
    next_definition.spec.name = "Sylvander revised".into();
    next_definition.spec.model.model_name = "model-b".into();
    next_definition.spec.model.allowed_models = vec![
        ModelSelection {
            provider_id: "primary".into(),
            model_id: "model-a".into(),
        },
        ModelSelection {
            provider_id: "primary".into(),
            model_id: "model-b".into(),
        },
    ];
    next_definition.spec.persona.system_prompt = "revision two prompt".into();
    next_definition.access = crate::config::AgentAccessConfig::default();
    let administrator = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal {
            id: sylvander_protocol::PrincipalId::new("operator"),
            kind: sylvander_protocol::PrincipalKind::User,
            authentication: sylvander_protocol::AuthenticationMethod::Internal,
            roles: vec!["admin".into()],
        },
        "admin-console",
        "internal",
        "hot-activate",
    );
    let mut uncomposable = next_definition.clone();
    uncomposable.prompt_profiles = vec![crate::config::PromptProfileConfig {
        id: "wrong-provider".into(),
        qualified_models: vec![sylvander_protocol::ModelSelection {
            provider_id: "another-provider".into(),
            model_id: "model-a".into(),
        }],
        system_prompt: "must not persist".into(),
    }];
    uncomposable.default_prompt_profile = Some("wrong-provider".into());
    let rejected = sylvander_channel::UiService::agent_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::AgentAdminRequest::UpdateDefinition {
            expected_active_revision: original_revision,
            definition: Box::new(
                crate::agent_admin::tests::draft_from_definition(&uncomposable).unwrap(),
            ),
        },
    )
    .await;
    assert!(
        matches!(
            rejected,
            sylvander_protocol::AgentAdminResponse::Error {
                error: sylvander_protocol::AgentAdminError {
                    code: sylvander_protocol::AgentAdminErrorCode::InvalidDefinition,
                    ..
                }
            }
        ),
        "unexpected rejection response: {rejected:?}"
    );
    let inspected = sylvander_channel::UiService::agent_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::AgentAdminRequest::ListRevisions {
            agent_id: next_definition.spec.id.clone(),
            before_revision: None,
            limit: 10,
        },
    )
    .await;
    assert!(matches!(
        inspected,
        sylvander_protocol::AgentAdminResponse::Success { result }
            if matches!(
                result.as_ref(),
                sylvander_protocol::AgentAdminResult::RevisionsListed {
                    active_revision,
                    revisions,
                    ..
                } if *active_revision == original_revision && revisions.len() == 1
            )
    ));
    let update_request = sylvander_protocol::AgentAdminRequest::UpdateDefinition {
        expected_active_revision: original_revision,
        definition: Box::new(
            crate::agent_admin::tests::draft_from_definition(&next_definition).unwrap(),
        ),
    };
    let update_message = sylvander_protocol::UiClientMessage::AgentAdmin {
        request: update_request.clone(),
    };
    let denial = sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &owner,
        &update_message,
    )
    .await
    .expect_err("ordinary session owners must not administer Agents");
    assert_eq!(
        denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &administrator,
        &update_message,
    )
    .await
    .expect("administrators may reach the Agent administration service");
    let registry_request = sylvander_protocol::RegistryAdminRequest::InspectProviderRevision {
        provider_id: "primary".into(),
        revision: 1,
    };
    let registry_message = sylvander_protocol::UiClientMessage::RegistryAdmin {
        request: registry_request.clone(),
    };
    assert!(
        sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &owner,
            &registry_message,
        )
        .await
        .is_err()
    );
    sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &administrator,
        &registry_message,
    )
    .await
    .expect("administrators may reach the registry administration seam");
    let unauthorized_registry = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &owner,
        registry_request.clone(),
    )
    .await;
    assert!(matches!(
        unauthorized_registry,
        sylvander_protocol::RegistryAdminResponse::Error { error }
            if error.code == sylvander_protocol::RegistryAdminErrorCode::Unauthorized
    ));
    let inspected_provider = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        registry_request,
    )
    .await;
    assert!(matches!(
        inspected_provider,
        sylvander_protocol::RegistryAdminResponse::Success { result }
            if matches!(
                result.as_ref(),
                sylvander_protocol::RegistryAdminResult::ProviderRevisionInspected {
                    revision
                } if revision.definition.provider_id == "primary"
                    && revision.definition.revision == 1
            )
    ));
    let missing_provider_revision = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::InspectProviderRevision {
            provider_id: "primary".into(),
            revision: 99,
        },
    )
    .await;
    assert!(matches!(
        missing_provider_revision,
        sylvander_protocol::RegistryAdminResponse::Error { error }
            if error.code == sylvander_protocol::RegistryAdminErrorCode::UnknownRevision
    ));
    let primary_binding = runtime
        .revision_provider
        .as_ref()
        .unwrap()
        .registry
        .load_active_provider("primary")
        .await
        .unwrap()
        .unwrap()
        .definition
        .credential_binding_id;
    let create_provider = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::CreateProvider {
            provider_id: "secondary".into(),
            definition: sylvander_protocol::ProviderDefinitionDraft {
                kind: "anthropic_compatible".into(),
                base_url: model_server.uri(),
                credential_binding_id: primary_binding,
            },
        },
    )
    .await;
    assert!(matches!(
        create_provider,
        sylvander_protocol::RegistryAdminResponse::Success { .. }
    ));
    let create_model = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::CreateModel {
            provider_id: "secondary".into(),
            model_id: "model-c".into(),
            definition: sylvander_protocol::ModelDefinitionDraft {
                context_window: 100_000,
                max_output_tokens: 4096,
                capabilities: vec!["tool_use".into()],
                lifecycle: sylvander_protocol::ModelLifecycleDraft::Active {},
                pricing: None,
            },
        },
    )
    .await;
    assert!(matches!(
        create_model,
        sylvander_protocol::RegistryAdminResponse::Success { .. }
    ));
    let binding_id = "credential/runtime-audit";
    let create_credential = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::CreateCredentialBinding {
            binding_id: binding_id.into(),
            reference: sylvander_protocol::CredentialSecretReferenceDraft::File {
                path: secret.display().to_string(),
            },
        },
    )
    .await;
    assert!(matches!(
        create_credential,
        sylvander_protocol::RegistryAdminResponse::Success { .. }
    ));
    let stage_credential = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::StageCredentialGeneration {
            binding_id: binding_id.into(),
            generation: 2,
            expected_active_generation: 1,
            reference: sylvander_protocol::CredentialSecretReferenceDraft::File {
                path: secret.display().to_string(),
            },
        },
    )
    .await;
    assert!(matches!(
        stage_credential,
        sylvander_protocol::RegistryAdminResponse::Success { .. }
    ));
    let activate_credential = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::ActivateCredentialGeneration {
            binding_id: binding_id.into(),
            generation: 2,
            expected_active_generation: 1,
        },
    )
    .await;
    assert!(matches!(
        activate_credential,
        sylvander_protocol::RegistryAdminResponse::Success { .. }
    ));
    let conflict = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::RollbackCredentialGeneration {
            binding_id: binding_id.into(),
            target_generation: 1,
            expected_active_generation: 1,
        },
    )
    .await;
    assert!(matches!(
        conflict,
        sylvander_protocol::RegistryAdminResponse::Error { error }
            if error.code
                == sylvander_protocol::RegistryAdminErrorCode::ActiveGenerationConflict
    ));
    let rollback_credential = sylvander_channel::UiService::registry_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::RegistryAdminRequest::RollbackCredentialGeneration {
            binding_id: binding_id.into(),
            target_generation: 1,
            expected_active_generation: 2,
        },
    )
    .await;
    assert!(matches!(
        rollback_credential,
        sylvander_protocol::RegistryAdminResponse::Success { .. }
    ));
    let registry_audits = evidence.administration_audits(20).await.unwrap();
    assert!(registry_audits.iter().any(|audit| {
        audit.operation == "inspect_provider_revision"
            && audit.resource_kind == "provider"
            && audit.resource_digest != "primary"
            && audit.version == Some(1)
            && audit.outcome == "succeeded"
    }));
    assert!(registry_audits.iter().any(|audit| {
        audit.operation == "inspect_provider_revision"
            && audit.version == Some(99)
            && audit.outcome == "failed"
            && audit.error_code.as_deref() == Some("unknown_revision")
    }));
    for (operation, resource_kind, version) in [
        ("create_provider", "provider", 1),
        ("create_model", "model", 1),
    ] {
        assert!(registry_audits.iter().any(|audit| {
            audit.operation == operation
                && audit.resource_kind == resource_kind
                && audit.version == Some(version)
                && audit.outcome == "succeeded"
        }));
    }
    for (operation, version, outcome) in [
        ("create_credential_binding", 1, "succeeded"),
        ("stage_credential_generation", 2, "succeeded"),
        ("activate_credential_generation", 2, "succeeded"),
        ("rollback_credential_generation", 1, "succeeded"),
    ] {
        assert!(registry_audits.iter().any(|audit| {
            audit.operation == operation
                && audit.resource_kind == "credential"
                && audit.resource_digest != binding_id
                && audit.version == Some(version)
                && audit.outcome == outcome
        }));
    }
    assert!(registry_audits.iter().any(|audit| {
        audit.operation == "rollback_credential_generation"
            && audit.version == Some(1)
            && audit.outcome == "failed"
            && audit.error_code.as_deref() == Some("active_generation_conflict")
    }));
    assert!(
        registry_audits
            .iter()
            .all(|audit| audit.outcome != "pending")
    );
    let admin_denials = evidence.authorization_denials(20).await.unwrap();
    assert!(
        admin_denials
            .iter()
            .any(|denial| denial.operation == "agent_admin")
    );
    assert!(
        admin_denials
            .iter()
            .any(|denial| denial.operation == "registry_admin")
    );
    let updated = sylvander_channel::UiService::agent_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        update_request,
    )
    .await;
    assert!(matches!(
        updated,
        sylvander_protocol::AgentAdminResponse::Success { result }
            if matches!(
                result.as_ref(),
                sylvander_protocol::AgentAdminResult::DefinitionUpdated { revision }
                    if revision.definition.revision == next_definition.revision
                        && !revision.active
            )
    ));
    let activated = sylvander_channel::UiService::agent_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::AgentAdminRequest::ActivateRevision {
            agent_id: next_definition.spec.id.clone(),
            revision: next_definition.revision,
            expected_active_revision: original_revision,
        },
    )
    .await;
    assert!(matches!(
        activated,
        sylvander_protocol::AgentAdminResponse::Success { result }
            if matches!(
                result.as_ref(),
                sylvander_protocol::AgentAdminResult::RevisionActivated {
                    active_revision,
                    ..
                } if *active_revision == next_definition.revision
            )
    ));
    let discovered =
        sylvander_channel::UiService::discover_agents(runtime.ui_service.as_ref(), &administrator)
            .await
            .unwrap();
    assert_eq!(discovered[0].revision, next_definition.revision);
    assert_eq!(discovered[0].name, next_definition.spec.name);
    let activated_session = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &administrator,
        SessionCreateRequest {
            agent_id: next_definition.spec.id.clone(),
            label: "hot activated revision".into(),
            channel_id: Some("admin-console".into()),
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        activated_session.effective.agent_revision, next_definition.revision,
        "new sessions must bind the hot-activated revision"
    );
    let provider = runtime.revision_provider.as_ref().unwrap();
    let original_run = provider
        .configured_revision(&next_definition.spec.id, original_revision)
        .await
        .unwrap()
        .run;
    let activated_run = provider
        .configured_revision(&next_definition.spec.id, next_definition.revision)
        .await
        .unwrap()
        .run;
    tokio::time::timeout(tokio::time::Duration::from_secs(1), async {
        loop {
            if original_run
                .get_session(&created.session_id)
                .await
                .is_some()
                && activated_run
                    .get_session(&activated_session.session_id)
                    .await
                    .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("revision workers must receive only their bound sessions");
    assert!(
        activated_run
            .get_session(&created.session_id)
            .await
            .is_none(),
        "an existing session must not drift to the activated revision"
    );
    let original_user = runtime
        .session_store
        .get(&created.session_id)
        .await
        .unwrap()
        .unwrap()
        .metadata
        .user_id;
    let activated_user = runtime
        .session_store
        .get(&activated_session.session_id)
        .await
        .unwrap()
        .unwrap()
        .metadata
        .user_id;
    let mut original_probe = sylvander_protocol::BusMessage::user_chat(
        created.session_id.clone(),
        original_user,
        "revision-one-probe",
    );
    original_probe.recipient =
        sylvander_protocol::Recipient::Agent(next_definition.spec.id.clone());
    runtime.bus().publish(original_probe).await.unwrap();
    let mut activated_probe = sylvander_protocol::BusMessage::user_chat(
        activated_session.session_id.clone(),
        activated_user,
        "revision-two-probe",
    );
    activated_probe.recipient =
        sylvander_protocol::Recipient::Agent(next_definition.spec.id.clone());
    runtime.bus().publish(activated_probe).await.unwrap();
    let revision_requests = tokio::time::timeout(tokio::time::Duration::from_secs(2), async {
        loop {
            let observed = model_server
                .received_requests()
                .await
                .unwrap()
                .into_iter()
                .filter_map(|request| {
                    let body: serde_json::Value = serde_json::from_slice(&request.body).ok()?;
                    let messages = body.get("messages")?.to_string();
                    let probe = ["revision-one-probe", "revision-two-probe"]
                        .into_iter()
                        .find(|probe| messages.contains(probe))?;
                    let model = body.get("model")?.as_str()?.to_owned();
                    let prompt = body
                        .get("system")?
                        .as_array()?
                        .first()?
                        .get("text")?
                        .as_str()?
                        .to_owned();
                    Some((probe.to_owned(), model, prompt))
                })
                .collect::<Vec<_>>();
            if observed.len() == 2 {
                break observed;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both revision-bound requests must reach the model provider");
    for (probe, model, configured_prompt) in [
        ("revision-one-probe", "model-a", "revision one prompt"),
        ("revision-two-probe", "model-b", "revision two prompt"),
    ] {
        assert!(
            revision_requests.iter().any(|request| {
                let safety = request
                    .2
                    .find(sylvander_agent::prompt::SHARED_SAFETY_PROMPT);
                let configured = request.2.find(configured_prompt);
                request.0 == probe
                    && request.1 == model
                    && matches!(
                        (safety, configured),
                        (Some(safety), Some(configured)) if safety < configured
                    )
            }),
            "missing revision-bound prompt {configured_prompt:?} for {probe:?}: {revision_requests:#?}"
        );
    }

    let stale_activation = sylvander_channel::UiService::agent_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::AgentAdminRequest::ActivateRevision {
            agent_id: next_definition.spec.id.clone(),
            revision: original_revision,
            expected_active_revision: original_revision,
        },
    )
    .await;
    assert!(matches!(
        stale_activation,
        sylvander_protocol::AgentAdminResponse::Error {
            error: sylvander_protocol::AgentAdminError {
                code: sylvander_protocol::AgentAdminErrorCode::RevisionConflict,
                ..
            }
        }
    ));
    let after_conflict =
        sylvander_channel::UiService::discover_agents(runtime.ui_service.as_ref(), &administrator)
            .await
            .unwrap();
    assert_eq!(
        after_conflict[0].revision, next_definition.revision,
        "an optimistic conflict must not move the active revision"
    );

    let rolled_back = sylvander_channel::UiService::agent_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::AgentAdminRequest::RollbackRevision {
            agent_id: next_definition.spec.id.clone(),
            target_revision: original_revision,
            expected_active_revision: next_definition.revision,
        },
    )
    .await;
    assert!(matches!(
        rolled_back,
        sylvander_protocol::AgentAdminResponse::Success { result }
            if matches!(
                result.as_ref(),
                sylvander_protocol::AgentAdminResult::RevisionRolledBack {
                    active_revision,
                    ..
                } if *active_revision == original_revision
            )
    ));
    let rolled_back_session = sylvander_channel::UiService::create_session(
        runtime.ui_service.as_ref(),
        &administrator,
        SessionCreateRequest {
            agent_id: next_definition.spec.id.clone(),
            label: "hot rolled back revision".into(),
            channel_id: Some("admin-console".into()),
            overrides: SessionConfigOverrides::default(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        rolled_back_session.effective.agent_revision, original_revision,
        "rollback must affect new sessions without restarting"
    );
    let reactivated = sylvander_channel::UiService::agent_admin(
        runtime.ui_service.as_ref(),
        &administrator,
        sylvander_protocol::AgentAdminRequest::ActivateRevision {
            agent_id: next_definition.spec.id.clone(),
            revision: next_definition.revision,
            expected_active_revision: original_revision,
        },
    )
    .await;
    assert!(matches!(
        reactivated,
        sylvander_protocol::AgentAdminResponse::Success { result }
            if matches!(
                result.as_ref(),
                sylvander_protocol::AgentAdminResult::RevisionActivated {
                    active_revision,
                    ..
                } if *active_revision == next_definition.revision
            )
    ));
    let administration_audits = evidence.agent_administration_audits(10).await.unwrap();
    assert_eq!(administration_audits.len(), 6);
    assert!(
        administration_audits
            .iter()
            .all(|audit| audit.principal_digest != "operator" && audit.agent_digest != "assistant")
    );
    assert_eq!(
        administration_audits
            .iter()
            .filter(|audit| audit.outcome == "succeeded")
            .count(),
        4
    );
    assert_eq!(
        administration_audits
            .iter()
            .filter(|audit| audit.outcome == "failed")
            .count(),
        2
    );
    assert!(administration_audits.iter().any(|audit| {
        audit.operation == "activate_revision"
            && audit.revision == original_revision
            && audit.expected_active_revision == original_revision
            && audit.outcome == "failed"
            && audit.error_code.as_deref()
                == Some(
                    agent_admin_error_code(
                        sylvander_protocol::AgentAdminErrorCode::RevisionConflict,
                    )
                    .as_str(),
                )
    }));
    let owner_denial = sylvander_channel::UiService::authorize_message(
        runtime.ui_service.as_ref(),
        &owner,
        &sylvander_protocol::UiClientMessage::GetSessionConfig {
            session_id: created.session_id.0.clone(),
        },
    )
    .await
    .expect_err("activating a restrictive Agent policy must revoke existing access");
    assert_eq!(
        owner_denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    assert!(
        sylvander_channel::UiService::discover_agents(runtime.ui_service.as_ref(), &owner)
            .await
            .unwrap()
            .is_empty()
    );
    let direct_denial = sylvander_channel::UiService::session_config(
        runtime.ui_service.as_ref(),
        &owner,
        &created.session_id,
    )
    .await
    .expect_err("direct session reads must enforce the active Agent policy");
    assert_eq!(
        direct_denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );
    let feedback_denial = sylvander_channel::UiService::submit_feedback(
        runtime.ui_service.as_ref(),
        &owner,
        RunFeedback {
            target: crate::evidence::feedback_target("feedback-auth-run", "feedback-auth-turn"),
            rating: sylvander_protocol::FeedbackRating::Positive,
            note: None,
            correction: None,
            tags: Vec::new(),
            task_result: None,
            artifacts: Vec::new(),
            validations: Vec::new(),
            privacy_class: sylvander_protocol::FeedbackPrivacyClass::Private,
        },
    )
    .await
    .expect_err("direct feedback writes must enforce the active Agent policy");
    assert_eq!(
        feedback_denial.code,
        sylvander_protocol::BoundaryErrorCode::Forbidden
    );

    for principal in [
        sylvander_protocol::AuthenticatedPrincipal {
            id: sylvander_protocol::PrincipalId::new("operator"),
            kind: sylvander_protocol::PrincipalKind::User,
            authentication: sylvander_protocol::AuthenticationMethod::Internal,
            roles: vec!["admin".into()],
        },
        sylvander_protocol::AuthenticatedPrincipal {
            id: sylvander_protocol::PrincipalId::new("runtime"),
            kind: sylvander_protocol::PrincipalKind::System,
            authentication: sylvander_protocol::AuthenticationMethod::Internal,
            roles: Vec::new(),
        },
    ] {
        let privileged = sylvander_protocol::BoundaryContext::authenticated(
            principal,
            "internal-control",
            "internal",
            uuid::Uuid::new_v4().to_string(),
        );
        sylvander_channel::UiService::authorize_message(
            runtime.ui_service.as_ref(),
            &privileged,
            &sylvander_protocol::UiClientMessage::GetSessionConfig {
                session_id: created.session_id.0.clone(),
            },
        )
        .await
        .expect("admin and system principals retain emergency access");
    }
    runtime.shutdown().await.unwrap();
    let counts = evidence.counts().await.unwrap();
    assert_eq!(
        counts.runs, 3,
        "two Runtime boots plus the explicit feedback fixture must be durable"
    );
    assert!(counts.events >= 1, "Agent lifecycle must reach evidence");

    let restarted = Runtime::boot_config(restart_config).await.unwrap();
    assert_eq!(
        restarted
            .configured_agent(&AgentId::new("assistant"))
            .unwrap()
            .definition
            .revision,
        next_definition.revision
    );
    let preserved = restarted
        .session_store
        .get(&created.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        preserved.effective_config.unwrap().agent_revision,
        original_revision,
        "activation must not migrate an existing session"
    );
    let (_, updated) = restarted
        .update_session_config(
            &created.session_id,
            preserved.config_revision,
            SessionConfigOverrides::default(),
        )
        .await
        .unwrap();
    assert_eq!(updated.agent_revision, original_revision);
    restarted.shutdown().await.unwrap();
}
