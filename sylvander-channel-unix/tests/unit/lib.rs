use super::*;
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use sylvander_agent::bus::{
    BusMessage, InProcessMessageBus, MessageBus, SubscriptionFilter, SystemMessage,
};
use sylvander_agent::session_store::{
    MessageRole, SessionLifetime, SessionStore, SqliteSessionStore, StoredSession,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn handle_client_msg(
    msg: ClientMsg,
    ctx: &ChannelContext,
    agent_id: &AgentId,
    tx: &mpsc::UnboundedSender<ServerMsg>,
    runtime: &RuntimeInfo,
) {
    let hub = Arc::new(Mutex::new(RelayHub::default()));
    hub.lock().await.clients.insert(0, tx.clone());
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "unix-client",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "unix",
        "unix",
        "test-request",
    );
    handle_client_msg_for_client(
        msg,
        ClientHandler {
            boundary: &boundary,
            ctx,
            agent_id,
            tx,
            runtime,
            hub: &hub,
            client_id: 0,
            ui_protocol_version: sylvander_protocol::UI_PROTOCOL_MAX_VERSION,
        },
    )
    .await;
}

#[derive(Default)]
struct EmptyUiService {
    registry_authorizations: AtomicUsize,
    registry_dispatches: AtomicUsize,
    allow_registry: bool,
    session_config: Option<sylvander_protocol::SessionConfigState>,
    chat_bus: Option<Arc<dyn MessageBus>>,
    feedback_target: Option<sylvander_protocol::FeedbackTarget>,
    compaction: Option<sylvander_protocol::CompactionReport>,
    rollback_preview: Option<sylvander_protocol::WorkspaceRollbackPreview>,
    rollback_report: Option<sylvander_protocol::WorkspaceRollbackReport>,
    allow_delete: bool,
}

#[tokio::test]
async fn oversized_frame_is_rejected_before_deserialization() {
    let (mut client, server) = tokio::io::duplex(64);
    let mut reader = FramedRead::new(server, LinesCodec::new_with_max_length(4));
    client.write_all(b"12345\n").await.unwrap();

    assert!(reader.next().await.unwrap().is_err());
}

#[async_trait]
impl sylvander_channel::UiService for EmptyUiService {
    async fn authorize_message(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        message: &ClientMsg,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        if matches!(message, ClientMsg::RegistryAdmin { .. }) {
            self.registry_authorizations.fetch_add(1, Ordering::Relaxed);
        }
        if matches!(message, ClientMsg::RegistryAdmin { .. })
            && !self.allow_registry
            && !boundary
                .principal
                .as_ref()
                .is_some_and(|principal| principal.has_role("admin"))
        {
            return Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                "registry_admin",
            ));
        }
        Ok(())
    }

    async fn submit_chat(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: sylvander_channel::ExternalChatRequest,
    ) -> Result<sylvander_channel::SubmittedChat, sylvander_protocol::BoundaryError> {
        let bus = self
            .chat_bus
            .as_ref()
            .ok_or_else(|| sylvander_protocol::BoundaryError::forbidden(boundary, "submit_chat"))?;
        let principal = boundary.principal.as_ref().ok_or_else(|| {
            sylvander_protocol::BoundaryError::unauthenticated(boundary, "submit_chat")
        })?;
        let session_id = request
            .existing_session
            .unwrap_or_else(|| SessionId::new(uuid::Uuid::new_v4().to_string()));
        let events = bus
            .subscribe(SubscriptionFilter {
                session_ids: Some(vec![session_id.clone()]),
                recipients: None,
                kinds: None,
            })
            .await
            .map_err(|_| sylvander_protocol::BoundaryError::forbidden(boundary, "submit_chat"))?;
        bus.publish(BusMessage {
            session_id: session_id.clone(),
            sender: sylvander_agent::bus::Sender::User(principal.id.0.clone()),
            recipient: sylvander_agent::bus::Recipient::Agent(request.agent_id),
            kind: MessageKind::Chat,
            payload: request.text,
            attachments: request.attachments,
            timestamp: sylvander_agent::session::now_secs(),
            id: sylvander_agent::bus::MessageId::new(),
        })
        .await
        .map_err(|_| sylvander_protocol::BoundaryError::forbidden(boundary, "submit_chat"))?;
        Ok(sylvander_channel::SubmittedChat {
            session_id,
            feedback_target: self.feedback_target.clone(),
            events,
        })
    }

    async fn submit_control(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        message: ClientMsg,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        let bus = self.chat_bus.as_ref().ok_or_else(|| {
            sylvander_protocol::BoundaryError::forbidden(boundary, "submit_control")
        })?;
        let (session_id, system) = match message {
            ClientMsg::Approve {
                session_id,
                call_id,
                approved,
                scope,
                reason,
            } => (
                SessionId::new(session_id),
                SystemMessage::ApproveTool {
                    call_id,
                    approved,
                    scope,
                    reason,
                },
            ),
            ClientMsg::ResolvePlan {
                session_id,
                plan_id,
                decision,
            } => (
                SessionId::new(session_id),
                SystemMessage::ResolvePlan { plan_id, decision },
            ),
            ClientMsg::CancelTask {
                session_id,
                task_id,
            } => {
                let session_id = SessionId::new(session_id);
                (
                    session_id.clone(),
                    SystemMessage::CancelTask {
                        session_id,
                        task_id,
                    },
                )
            }
            _ => {
                return Err(sylvander_protocol::BoundaryError::forbidden(
                    boundary,
                    "submit_control",
                ));
            }
        };
        bus.publish(BusMessage {
            session_id,
            sender: sylvander_agent::bus::Sender::System,
            recipient: sylvander_agent::bus::Recipient::Agent(AgentId::new("agent-1")),
            kind: MessageKind::System(system),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: sylvander_agent::session::now_secs(),
            id: sylvander_agent::bus::MessageId::new(),
        })
        .await
        .map_err(|_| sylvander_protocol::BoundaryError::forbidden(boundary, "submit_control"))
    }

    async fn delete_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        _: &SessionId,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        if self.allow_delete {
            Ok(())
        } else {
            Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                "delete_session",
            ))
        }
    }

    async fn discover_agents(
        &self,
        _boundary: &sylvander_protocol::BoundaryContext,
    ) -> Result<Vec<sylvander_protocol::AgentDescriptor>, sylvander_protocol::BoundaryError> {
        Ok(Vec::new())
    }

    async fn create_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        _request: sylvander_protocol::SessionCreateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        Err(sylvander_protocol::BoundaryError::forbidden(
            boundary,
            "create_session",
        ))
    }

    async fn session_config(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        self.session_config.clone().ok_or_else(|| {
            sylvander_protocol::BoundaryError::forbidden(boundary, "get_session_config")
        })
    }

    async fn update_session_config(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        _request: sylvander_protocol::SessionConfigUpdateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        Err(sylvander_protocol::BoundaryError::forbidden(
            boundary,
            "update_session_config",
        ))
    }

    async fn submit_feedback(
        &self,
        _boundary: &sylvander_protocol::BoundaryContext,
        _feedback: sylvander_protocol::RunFeedback,
    ) -> Result<String, sylvander_protocol::BoundaryError> {
        Ok("feedback-1".into())
    }

    async fn memory_confirmation(
        &self,
        _boundary: &sylvander_protocol::BoundaryContext,
        request: sylvander_protocol::MemoryConfirmationRequest,
    ) -> sylvander_protocol::MemoryConfirmationResponse {
        match request {
            sylvander_protocol::MemoryConfirmationRequest::List { session_id, .. } => {
                sylvander_protocol::MemoryConfirmationResponse::Pending {
                    version: sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                    session_id,
                    confirmations: vec![sylvander_protocol::PendingMemoryConfirmation {
                        candidate_id: "candidate-1".into(),
                        expected_revision: 2,
                        scope: sylvander_protocol::MemoryConfirmationScope::UserProfile,
                        summary: "prefers concise answers".into(),
                    }],
                }
            }
            sylvander_protocol::MemoryConfirmationRequest::Decide {
                session_id,
                candidate_id,
                decision,
                ..
            } => sylvander_protocol::MemoryConfirmationResponse::Recorded {
                version: sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                session_id,
                candidate_id,
                decision,
            },
        }
    }

    fn identity_binding_capabilities(&self) -> sylvander_protocol::IdentityBindingCapabilities {
        sylvander_protocol::IdentityBindingCapabilities::current()
    }

    async fn identity_binding(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        identity: sylvander_channel::AuthenticatedTransportIdentity,
        _request: sylvander_protocol::IdentityBindingRequest,
    ) -> sylvander_protocol::IdentityBindingResponse {
        let (transport, instance, principal) = identity.into_parts();
        assert_eq!(transport, boundary.transport);
        assert_eq!(instance, boundary.channel_instance_id);
        assert_eq!(
            principal,
            boundary.principal.as_ref().expect("principal").id.0
        );
        sylvander_protocol::IdentityBindingResponse::Resolved {
            version: sylvander_protocol::IDENTITY_BINDING_PROTOCOL_VERSION,
            binding: sylvander_protocol::IdentityBindingView {
                user_id: sylvander_protocol::UserId::new("stable-user"),
                revision: 7,
                linked_at_unix_secs: 11,
            },
        }
    }

    async fn compact_session(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_protocol::CompactionReport, sylvander_protocol::BoundaryError> {
        self.compaction.clone().ok_or_else(|| {
            sylvander_protocol::BoundaryError::forbidden(boundary, "compact_session")
        })
    }

    async fn preview_workspace_rollback(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_protocol::WorkspaceRollbackPreview, sylvander_protocol::BoundaryError>
    {
        self.rollback_preview.clone().ok_or_else(|| {
            sylvander_protocol::BoundaryError::forbidden(boundary, "preview_workspace_rollback")
        })
    }

    async fn rollback_workspace(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        _session_id: &SessionId,
        _expected_turn_id: &str,
    ) -> Result<sylvander_protocol::WorkspaceRollbackReport, sylvander_protocol::BoundaryError>
    {
        self.rollback_report.clone().ok_or_else(|| {
            sylvander_protocol::BoundaryError::forbidden(boundary, "rollback_workspace")
        })
    }

    async fn registry_admin(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: sylvander_protocol::RegistryAdminRequest,
    ) -> sylvander_protocol::RegistryAdminResponse {
        self.registry_dispatches.fetch_add(1, Ordering::Relaxed);
        assert!(
            self.allow_registry
                || boundary
                    .principal
                    .as_ref()
                    .is_some_and(|principal| principal.has_role("admin")),
            "non-administrator reached registry dispatch"
        );
        let result = match request {
            sylvander_protocol::RegistryAdminRequest::InspectProviderRevision {
                provider_id,
                revision,
            } => sylvander_protocol::RegistryAdminResult::ProviderRevisionInspected {
                revision: sylvander_protocol::ProviderRevisionView {
                    definition: sylvander_protocol::RedactedProviderDefinition {
                        provider_id,
                        revision,
                        kind: "mock".into(),
                        base_url_sha256: "base-digest".into(),
                        credential_binding_id_sha256: "binding-digest".into(),
                    },
                    digest_sha256: "definition-digest".into(),
                    created_at_unix_secs: 7,
                    active: true,
                },
            },
            sylvander_protocol::RegistryAdminRequest::CreateCredentialBinding { .. } => {
                sylvander_protocol::RegistryAdminResult::CredentialBindingCreated {
                    generation: sylvander_protocol::CredentialGenerationView {
                        binding_id_sha256: "binding-id-digest".into(),
                        generation: 1,
                        reference_kind: sylvander_protocol::CredentialReferenceKind::Environment,
                        reference_configured: true,
                        reference_digest_sha256: "reference-digest".into(),
                        created_at_unix_secs: 7,
                        active: true,
                    },
                }
            }
            _ => unreachable!(),
        };
        sylvander_protocol::RegistryAdminResponse::Success {
            result: Box::new(result),
        }
    }
}

fn socket_path() -> PathBuf {
    PathBuf::from("/tmp").join(format!(
        "sylv-u-{}-{}.sock",
        std::process::id(),
        &uuid::Uuid::new_v4().to_string()[..8]
    ))
}

fn runtime_info() -> RuntimeInfo {
    RuntimeInfo {
        model: sylvander_protocol::ModelSelection {
            provider_id: "test".into(),
            model_id: "test-model".into(),
        },
        reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
        models: vec![sylvander_protocol::ModelDescriptor {
            id: "test-model".into(),
            provider: "test".into(),
            capabilities: 0b101,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
            lifecycle: sylvander_protocol::ModelLifecycle::Active,
            pricing: None,
        }],
        permissions: sylvander_protocol::PermissionProfile::default(),
        capabilities: 0b101,
        approval_enabled: true,
        max_attachment_bytes: 1024,
        platform: sylvander_protocol::PlatformSnapshot::default(),
        platform_provider: None,
    }
}

fn private_session_config(
    session_id: &str,
    prompt: &str,
    digest: &str,
) -> sylvander_protocol::SessionConfigState {
    use sylvander_protocol::{
        PromptLayerDigest, PromptLayerKind, PromptManifest, SessionConfigProvenance,
        SessionConfigSource, SessionConfigSourceKind, SessionEffectiveConfig,
    };
    let source = SessionConfigSource {
        kind: SessionConfigSourceKind::SessionOverride,
        reference: Some("session".into()),
    };
    sylvander_protocol::SessionConfigState {
        session_id: SessionId::new(session_id),
        revision: 2,
        overrides: sylvander_protocol::SessionConfigOverrides {
            system_prompt: Some(prompt.into()),
            ..sylvander_protocol::SessionConfigOverrides::default()
        },
        effective: SessionEffectiveConfig {
            agent_id: AgentId::new("agent-1"),
            agent_revision: 1,
            provider_id: "test".into(),
            provider_revision: 1,
            model_id: "test-model".into(),
            model_revision: 1,
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
            permissions: sylvander_protocol::PermissionProfile::default(),
            prompt_profile: None,
            system_prompt_sha256: digest.into(),
            prompt_manifest: PromptManifest {
                layers: vec![PromptLayerDigest {
                    kind: PromptLayerKind::SessionInput,
                    reference: Some("session".into()),
                    sha256: digest.into(),
                    byte_count: prompt.len() as u64,
                }],
                aggregate_sha256: "aggregate-digest".into(),
                total_bytes: prompt.len() as u64,
            },
            agent_workspace: None,
            user_workspace: None,
            workspace_mounts: Vec::new(),
            execution_target: "local".into(),
            provenance: SessionConfigProvenance {
                model: source.clone(),
                reasoning_effort: source.clone(),
                permissions: source.clone(),
                prompt_profile: source.clone(),
                system_prompt: source.clone(),
                agent_workspace: source.clone(),
                user_workspace: source.clone(),
                execution_target: source,
            },
        },
    }
}

async fn connect(path: &std::path::Path) -> tokio::net::UnixStream {
    for _ in 0..40 {
        if let Ok(stream) = tokio::net::UnixStream::connect(path).await {
            return stream;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("unix channel did not start");
}

async fn send_and_read(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    message: serde_json::Value,
) -> serde_json::Value {
    let line = send_and_read_wire(write, reader, message).await;
    serde_json::from_str(&line).expect("json response")
}

async fn send_and_read_wire(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    message: serde_json::Value,
) -> String {
    write
        .write_all(format!("{message}\n").as_bytes())
        .await
        .expect("write");
    tokio::time::timeout(std::time::Duration::from_secs(1), reader.next_line())
        .await
        .expect("response timeout")
        .expect("read")
        .expect("response")
}

async fn negotiate(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
) {
    let welcome = send_and_read(
        write,
        reader,
        serde_json::json!({
            "type":"hello",
            "protocol": {
                "client_name":"channel-test",
                "min_version":sylvander_protocol::UI_PROTOCOL_VERSION,
                "max_version":sylvander_protocol::UI_PROTOCOL_VERSION,
                "capabilities":[]
            }
        }),
    )
    .await;
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(
        welcome["protocol"]["version"],
        sylvander_protocol::UI_PROTOCOL_VERSION
    );
}

#[tokio::test]
async fn runtime_info_reports_server_truth() {
    let bus = Arc::new(InProcessMessageBus::new());
    let context = ChannelContext::with_services(
        bus,
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        None,
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::GetRuntimeInfo,
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;

    let response = rx.recv().await.expect("runtime response");
    assert!(matches!(
        response,
        ServerMsg::RuntimeInfo {
            model,
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
            models,
            permissions: sylvander_protocol::PermissionProfile {
                file_access: sylvander_protocol::FileAccess::WorkspaceWrite,
                network_access: sylvander_protocol::NetworkAccess::Denied,
                approval_policy: sylvander_protocol::ApprovalPolicy::Allow,
            },
            capabilities: 0b101,
            approval_enabled: true,
            max_attachment_bytes: 1024,
            ..
        } if model.provider_id == "test"
            && model.model_id == "test-model"
            && models.len() == 1
    ));
}

#[tokio::test]
async fn runtime_info_reads_fresh_platform_truth_for_each_request() {
    let bus = Arc::new(InProcessMessageBus::new());
    let context = ChannelContext::with_services(
        bus,
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        None,
        None,
    );
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let mut runtime = runtime_info();
    runtime.platform_provider = Some(Arc::new(move || {
        let generation = observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        sylvander_protocol::PlatformSnapshot {
            features: vec![sylvander_protocol::PlatformFeature {
                kind: sylvander_protocol::PlatformFeatureKind::Mcp,
                name: "search".into(),
                status: sylvander_protocol::PlatformFeatureStatus::Active,
                summary: format!("generation {generation}"),
                source: None,
                trust: None,
                auth: sylvander_protocol::PlatformAuthStatus::NotRequired,
                capabilities: vec!["tools".into()],
                reloadable: true,
            }],
            commands: Vec::new(),
            tool_presentations: Vec::new(),
        }
    }));
    let (tx, mut rx) = mpsc::unbounded_channel();

    for expected in ["generation 1", "generation 2"] {
        handle_client_msg(
            ClientMsg::GetRuntimeInfo,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime,
        )
        .await;
        let ServerMsg::RuntimeInfo { platform, .. } = rx.recv().await.expect("runtime response")
        else {
            panic!("expected runtime info");
        };
        assert_eq!(platform.features[0].summary, expected);
    }
}

#[tokio::test]
async fn agent_discovery_is_served_through_the_ui_service_boundary() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(EmptyUiService::default())),
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::DiscoverAgents,
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;

    assert!(matches!(
        rx.recv().await.expect("discovery response"),
        ServerMsg::AgentsDiscovered { agents } if agents.is_empty()
    ));

    handle_client_msg(
        ClientMsg::SubmitFeedback {
            feedback: sylvander_protocol::RunFeedback {
                target: sylvander_protocol::FeedbackTarget("sha256:target".into()),
                rating: sylvander_protocol::FeedbackRating::Positive,
                note: None,
                correction: None,
                tags: Vec::new(),
                task_result: None,
                artifacts: Vec::new(),
                validations: Vec::new(),
                privacy_class: sylvander_protocol::FeedbackPrivacyClass::Private,
            },
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;
    assert!(matches!(
        rx.recv().await.expect("feedback response"),
        ServerMsg::FeedbackRecorded { feedback_id } if feedback_id == "feedback-1"
    ));
}

#[tokio::test]
async fn identity_binding_round_trip_uses_authenticated_unix_ingress() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(EmptyUiService::default())),
        None,
    );
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "unix:local:uid:501",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "local",
        "unix",
        "identity-request",
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg_for_client(
        ClientMsg::IdentityBinding {
            request: Arc::new(sylvander_protocol::IdentityBindingRequest {
                version: sylvander_protocol::IDENTITY_BINDING_PROTOCOL_VERSION,
                action: sylvander_protocol::IdentityBindingAction::Resolve {},
            }),
        },
        ClientHandler {
            boundary: &boundary,
            ctx: &context,
            agent_id: &AgentId::new("agent-1"),
            tx: &tx,
            runtime: &runtime_info(),
            hub: &Arc::new(Mutex::new(RelayHub::default())),
            client_id: 1,
            ui_protocol_version: sylvander_protocol::UI_PROTOCOL_MAX_VERSION,
        },
    )
    .await;

    let response = rx.recv().await.expect("identity response");
    let encoded = serde_json::to_string(&response).expect("serialize once");
    let decoded: ServerMsg = serde_json::from_str(&encoded).expect("decode response");
    assert!(matches!(
        decoded,
        ServerMsg::IdentityBinding { response }
            if matches!(
                response.as_ref(),
                sylvander_protocol::IdentityBindingResponse::Resolved {
                    binding,
                    ..
                } if binding.user_id == sylvander_protocol::UserId::new("stable-user")
                    && binding.revision == 7
            )
    ));
}

#[tokio::test]
async fn agent_admin_without_ui_service_returns_content_free_error() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        None,
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::AgentAdmin {
            request: sylvander_protocol::AgentAdminRequest::InspectRevision {
                agent_id: AgentId::new("private-agent"),
                revision: 42,
            },
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;

    let response = rx.recv().await.expect("Agent admin response");
    let json = serde_json::to_string(&response).expect("serialize response");
    assert!(matches!(
        response,
        ServerMsg::AgentAdmin {
            response: sylvander_protocol::AgentAdminResponse::Error {
                error: sylvander_protocol::AgentAdminError {
                    code: sylvander_protocol::AgentAdminErrorCode::Unauthorized,
                    agent_id: None,
                    revision: None,
                    ..
                }
            }
        }
    ));
    assert!(!json.contains("private-agent"));
    assert!(!json.contains("42"));
}

#[tokio::test]
async fn registry_admin_without_ui_service_returns_content_free_error() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        None,
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::RegistryAdmin {
            request: sylvander_protocol::RegistryAdminRequest::InspectProviderRevision {
                provider_id: "private-provider".into(),
                revision: 42,
            },
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;

    let response = rx.recv().await.expect("registry admin response");
    let json = serde_json::to_string(&response).unwrap();
    assert!(matches!(
        response,
        ServerMsg::RegistryAdmin {
            response: sylvander_protocol::RegistryAdminResponse::Error {
                error: sylvander_protocol::RegistryAdminError {
                    code: sylvander_protocol::RegistryAdminErrorCode::Unauthorized,
                    provider_id: None,
                    revision: None,
                    ..
                }
            }
        }
    ));
    assert!(!json.contains("private-provider"));
    assert!(!json.contains("42"));
}

fn inspect_registry_request() -> ClientMsg {
    serde_json::from_value(serde_json::json!({
        "type": "registry_admin",
        "request": {
            "operation": "inspect_provider_revision",
            "provider_id": "provider-a",
            "revision": 9
        }
    }))
    .expect("decode registry request")
}

async fn dispatch_client_message_as(
    principal: sylvander_protocol::AuthenticatedPrincipal,
    request: ClientMsg,
) -> ServerMsg {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(EmptyUiService::default())),
        None,
    );
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        principal,
        "unix-test",
        "unix",
        "request-1",
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg_for_client(
        request,
        ClientHandler {
            boundary: &boundary,
            ctx: &context,
            agent_id: &AgentId::new("agent-1"),
            tx: &tx,
            runtime: &runtime_info(),
            hub: &Arc::new(Mutex::new(RelayHub::default())),
            client_id: 1,
            ui_protocol_version: sylvander_protocol::UI_PROTOCOL_MAX_VERSION,
        },
    )
    .await;
    rx.recv().await.expect("registry transport response")
}

#[tokio::test]
async fn registry_admin_round_trip_preserves_success_response() {
    let mut principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "admin",
        sylvander_protocol::AuthenticationMethod::UnixPeer,
    );
    principal.roles.push("admin".into());
    let response = dispatch_client_message_as(principal, inspect_registry_request()).await;
    let wire = serde_json::to_string(&response).expect("encode registry response");
    let decoded: ServerMsg = serde_json::from_str(&wire).expect("decode registry response");

    assert!(matches!(
        decoded,
        ServerMsg::RegistryAdmin {
            response: sylvander_protocol::RegistryAdminResponse::Success { result }
        } if matches!(
            result.as_ref(),
            sylvander_protocol::RegistryAdminResult::ProviderRevisionInspected { revision }
                if revision.definition.provider_id == "provider-a"
                    && revision.definition.revision == 9
        )
    ));
}

#[tokio::test]
async fn registry_admin_non_administrator_is_rejected_before_dispatch() {
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "reader",
        sylvander_protocol::AuthenticationMethod::UnixPeer,
    );
    assert!(matches!(
        dispatch_client_message_as(principal, inspect_registry_request()).await,
        ServerMsg::BoundaryDenied { error }
            if error.code == sylvander_protocol::BoundaryErrorCode::Forbidden
                && error.operation == "registry_admin"
    ));
}

#[test]
fn server_advertises_administration_capabilities() {
    let capabilities = ui_protocol_capabilities();
    assert!(
        capabilities
            .iter()
            .any(|item| item == sylvander_protocol::IDENTITY_BINDING_CAPABILITY)
    );
    assert!(
        capabilities
            .iter()
            .any(|item| item == "credential_registry_lifecycle")
    );
    assert!(
        capabilities
            .iter()
            .any(|item| item == "agent_administration")
    );
    assert!(
        capabilities
            .iter()
            .any(|item| item == "registry_administration")
    );
    assert!(
        capabilities
            .iter()
            .any(|item| item == "provider_model_registry_lifecycle")
    );
    assert!(
        capabilities
            .iter()
            .any(|item| item == "credential_registry_lifecycle")
    );
}

#[tokio::test]
async fn current_protocol_is_required_before_registry_mutation_dispatch() {
    let path = socket_path();
    let service = Arc::new(EmptyUiService {
        allow_registry: true,
        ..EmptyUiService::default()
    });
    let channel = Arc::new(UnixChannel::new(&path, "agent-1"));
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(service.clone()),
        None,
    )));
    let mutation = serde_json::json!({
        "type": "registry_admin",
        "request": {
            "operation": "create_credential_binding",
            "binding_id": "credential/private-binding",
            "reference": {"source": "environment", "name": "PRIVATE_API_KEY"}
        }
    });

    let stream = connect(&path).await;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let first = send_and_read(&mut write, &mut lines, mutation.clone()).await;
    assert_eq!(first["error"]["code"], "handshake_required");
    assert_eq!(service.registry_authorizations.load(Ordering::Relaxed), 0);
    assert_eq!(service.registry_dispatches.load(Ordering::Relaxed), 0);

    let stream = connect(&path).await;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let old_hello = serde_json::json!({
        "type": "hello",
        "protocol": {
            "client_name": "old-client",
            "min_version": sylvander_protocol::UI_PROTOCOL_VERSION - 1,
            "max_version": sylvander_protocol::UI_PROTOCOL_VERSION - 1,
            "capabilities": []
        }
    });
    let rejected = send_and_read(&mut write, &mut lines, old_hello).await;
    assert_eq!(rejected["error"]["code"], "incompatible_protocol");
    let rejected_wire = rejected.to_string();
    assert!(!rejected_wire.contains("credential/private-binding"));
    assert!(!rejected_wire.contains("PRIVATE_API_KEY"));
    assert_eq!(service.registry_authorizations.load(Ordering::Relaxed), 0);
    assert_eq!(service.registry_dispatches.load(Ordering::Relaxed), 0);

    let stream = connect(&path).await;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let welcome = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type": "hello",
            "protocol": {
                "client_name": "current-client",
                "min_version": sylvander_protocol::UI_PROTOCOL_VERSION,
                "max_version": sylvander_protocol::UI_PROTOCOL_VERSION,
                "capabilities": []
            }
        }),
    )
    .await;
    assert_eq!(
        welcome["protocol"]["version"],
        sylvander_protocol::UI_PROTOCOL_VERSION
    );
    let duplicate = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type": "hello",
            "protocol": {
                "client_name": "current-client",
                "min_version": sylvander_protocol::UI_PROTOCOL_VERSION,
                "max_version": sylvander_protocol::UI_PROTOCOL_VERSION,
                "capabilities": []
            }
        }),
    )
    .await;
    assert_eq!(duplicate["error"]["code"], "duplicate_handshake");
    let accepted = send_and_read(&mut write, &mut lines, mutation).await;
    assert_eq!(accepted["type"], "registry_admin");
    assert_eq!(accepted["response"]["status"], "success");
    assert_eq!(service.registry_authorizations.load(Ordering::Relaxed), 1);
    assert_eq!(service.registry_dispatches.load(Ordering::Relaxed), 1);

    task.abort();
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn memory_confirmation_round_trips_over_a_real_unix_socket() {
    let path = socket_path();
    let channel = Arc::new(UnixChannel::new(&path, "agent-1"));
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(EmptyUiService::default())),
        None,
    )));
    let stream = connect(&path).await;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    negotiate(&mut write, &mut lines).await;

    let pending = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type": "memory_confirmation",
            "request": {
                "operation": "list",
                "version": sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                "session_id": "session-1"
            }
        }),
    )
    .await;
    assert_eq!(pending["type"], "memory_confirmation");
    assert_eq!(pending["response"]["result"], "pending");
    assert_eq!(
        pending["response"]["confirmations"][0]["summary"],
        "prefers concise answers"
    );
    let wire = pending.to_string();
    assert!(!wire.contains("user_id"));
    assert!(!wire.contains("agent_id"));

    let recorded = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type": "memory_confirmation",
            "request": {
                "operation": "decide",
                "version": sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                "session_id": "session-1",
                "candidate_id": "candidate-1",
                "expected_revision": 2,
                "decision": "reject"
            }
        }),
    )
    .await;
    assert_eq!(recorded["response"]["result"], "recorded");
    assert_eq!(recorded["response"]["decision"], "reject");

    task.abort();
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn session_prompt_is_redacted_on_the_unix_wire() {
    const SENTINEL: &str = "UNIX_PRIVATE_SESSION_PROMPT_SENTINEL";
    const DIGEST: &str = "unix-public-prompt-digest";
    let path = socket_path();
    let service = Arc::new(EmptyUiService {
        session_config: Some(private_session_config("session-secret", SENTINEL, DIGEST)),
        ..EmptyUiService::default()
    });
    let channel = Arc::new(UnixChannel::new(&path, "agent-1"));
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(service),
        None,
    )));
    let stream = connect(&path).await;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    negotiate(&mut write, &mut lines).await;

    let wire = send_and_read_wire(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type": "get_session_config",
            "session_id": "session-secret"
        }),
    )
    .await;
    let response: serde_json::Value = serde_json::from_str(&wire).expect("session config");

    assert!(!wire.contains(SENTINEL));
    assert!(
        response["state"]["overrides"]
            .get("system_prompt")
            .is_none()
    );
    assert_eq!(
        response["state"]["effective"]["system_prompt_sha256"],
        DIGEST
    );
    assert_eq!(
        response["state"]["effective"]["prompt_manifest"]["layers"][0]["sha256"],
        DIGEST
    );

    task.abort();
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn credential_create_round_trip_returns_only_redacted_view() {
    let binding_id = "credential/private-binding";
    let locator = "PRIVATE_PROVIDER_API_KEY";
    let request: ClientMsg = serde_json::from_value(serde_json::json!({
        "type": "registry_admin",
        "request": {
            "operation": "create_credential_binding",
            "binding_id": binding_id,
            "reference": {
                "source": "environment",
                "name": locator
            }
        }
    }))
    .expect("decode credential create request");
    let mut principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "admin",
        sylvander_protocol::AuthenticationMethod::UnixPeer,
    );
    principal.roles.push("admin".into());

    let response = dispatch_client_message_as(principal, request).await;
    let wire = serde_json::to_string(&response).expect("encode credential response");
    assert!(!wire.contains(binding_id));
    assert!(!wire.contains(locator));
    assert!(matches!(
        response,
        ServerMsg::RegistryAdmin {
            response: sylvander_protocol::RegistryAdminResponse::Success { result }
        } if matches!(
            result.as_ref(),
            sylvander_protocol::RegistryAdminResult::CredentialBindingCreated { generation }
                if generation.generation == 1
                    && generation.reference_configured
                    && generation.binding_id_sha256 == "binding-id-digest"
        )
    ));
}

#[tokio::test]
async fn model_selection_without_session_fails_closed() {
    let bus = Arc::new(InProcessMessageBus::new());
    let context = ChannelContext::with_services(
        bus,
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(EmptyUiService::default())),
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::SelectModel {
            session_id: None,
            model: sylvander_protocol::ModelSelection {
                provider_id: "test".into(),
                model_id: "thinking-model".into(),
            },
            reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;

    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::OperationError { operation, message })
            if operation == "select_model" && message.contains("session_id")
    ));

    handle_client_msg(
        ClientMsg::SelectPermissions {
            session_id: None,
            profile: sylvander_protocol::PermissionProfile {
                file_access: sylvander_protocol::FileAccess::ReadOnly,
                network_access: sylvander_protocol::NetworkAccess::Denied,
                approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
            },
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;
    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::OperationError { operation, message })
            if operation == "select_permissions" && message.contains("session_id")
    ));

    handle_client_msg(
        ClientMsg::Compact {
            session_id: "missing-session".into(),
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;
    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::CompactionStarted {
            automatic: false,
            ..
        })
    ));
    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::CompactionFailed {
            automatic: false,
            reason,
            ..
        }) if reason == "the principal is not allowed to access this resource"
    ));
}

#[tokio::test]
async fn workspace_rollback_preview_and_confirmation_round_trip() {
    let bus = Arc::new(InProcessMessageBus::new());
    let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
    let ui = EmptyUiService {
        rollback_preview: Some(sylvander_protocol::WorkspaceRollbackPreview {
            turn_id: "turn-1".into(),
            files: vec!["file.txt".into()],
        }),
        rollback_report: Some(sylvander_protocol::WorkspaceRollbackReport {
            turn_id: "turn-1".into(),
            restored: vec!["file.txt".into()],
        }),
        ..EmptyUiService::default()
    };
    let context = ChannelContext::with_services(
        bus,
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
        Some(Arc::new(ui)),
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::PreviewWorkspaceRollback {
            session_id: session_id.0.clone(),
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;
    let turn_id = match rx.recv().await.unwrap() {
        ServerMsg::WorkspaceRollbackPreview { preview, .. } => preview.turn_id,
        other => panic!("unexpected preview response: {other:?}"),
    };
    handle_client_msg(
        ClientMsg::RollbackWorkspace {
            session_id: session_id.0.clone(),
            expected_turn_id: turn_id,
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &runtime_info(),
    )
    .await;
    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::WorkspaceRollbackCompleted { .. })
    ));
}

#[tokio::test]
async fn persisted_session_load_rename_fork_and_archive_round_trip() {
    let path = socket_path();
    let agent_id = AgentId::new("agent-1");
    let store: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store"));
    let session_id = SessionId::new("session-1");
    let credential_probe = tempfile::NamedTempFile::new().expect("credential probe");
    let principal_id = format!(
        "unix:unix:uid:{}",
        credential_probe
            .as_file()
            .metadata()
            .expect("credential metadata")
            .uid()
    );
    let metadata = sylvander_agent::session::SessionMetadata {
        workspace: "/workspace/project".into(),
        name: "Original".into(),
        user_id: principal_id.clone(),
    };
    store
        .save(&StoredSession::new(
            session_id.clone(),
            "Original",
            SessionLifetime::Persistent,
            metadata,
            vec![agent_id.clone()],
        ))
        .await
        .expect("save");
    let caller = unix_session_context(&principal_id, &agent_id, session_id.clone());
    store
        .append_message(
            &caller,
            &session_id,
            MessageRole::User,
            serde_json::json!({"role":"user","content":"hello"}),
            None,
            None,
            None,
        )
        .await
        .expect("append");
    for (role, content) in [
        (MessageRole::Assistant, "answer one"),
        (MessageRole::User, "question two"),
        (MessageRole::Assistant, "answer two"),
    ] {
        store
                .append_message(
                    &caller,
                    &session_id,
                    role,
                    serde_json::json!({"role": match role { MessageRole::User => "user", _ => "assistant" }, "content": content}),
                    None,
                    None,
                    None,
                )
                .await
                .expect("append turn");
    }
    store
        .record_usage(&session_id, 120, 30, Some(45_000))
        .await
        .expect("usage");

    let channel = Arc::new(UnixChannel::new(&path, agent_id));
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        store.clone(),
        Some(Arc::new(EmptyUiService {
            allow_delete: true,
            ..EmptyUiService::default()
        })),
        None,
    );
    let task = tokio::spawn(channel.run(context));
    let stream = connect(&path).await;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    negotiate(&mut write, &mut lines).await;

    let loaded = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({"type":"load_session","session_id":"session-1"}),
    )
    .await;
    assert_eq!(loaded["type"], "session_history");
    assert_eq!(loaded["messages"][0]["text"], "hello");
    assert_eq!(loaded["iterations"], 1);
    assert_eq!(loaded["input_tokens"], 120);
    assert_eq!(loaded["output_tokens"], 30);

    let renamed = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type":"rename_session",
            "session_id":"session-1",
            "label":"Renamed"
        }),
    )
    .await;
    assert_eq!(renamed["label"], "Renamed");

    let forked = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({"type":"fork_session","session_id":"session-1"}),
    )
    .await;
    assert_eq!(forked["type"], "session_history");
    assert_ne!(forked["session"]["id"], "session-1");
    assert_eq!(forked["messages"][0]["text"], "hello");
    assert_eq!(forked["source_session_id"], "session-1");

    let checkpoint = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type":"fork_session",
            "session_id":"session-1",
            "checkpoint":true
        }),
    )
    .await;
    assert!(
        checkpoint["session"]["label"]
            .as_str()
            .unwrap()
            .contains("checkpoint")
    );
    assert!(
        checkpoint["notice"]
            .as_str()
            .unwrap()
            .contains("workspace files unchanged")
    );

    let rewound = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type":"fork_session",
            "session_id":"session-1",
            "completed_turns":1
        }),
    )
    .await;
    assert_eq!(rewound["type"], "session_history");
    assert_eq!(rewound["messages"].as_array().unwrap().len(), 2);
    assert!(
        rewound["session"]["label"]
            .as_str()
            .unwrap()
            .contains("rewind 1")
    );
    assert!(
        rewound["notice"]
            .as_str()
            .unwrap()
            .contains("workspace files unchanged")
    );
    let invalid_rewind = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type":"fork_session",
            "session_id":"session-1",
            "completed_turns":99
        }),
    )
    .await;
    assert_eq!(invalid_rewind["type"], "operation_error");
    assert_eq!(invalid_rewind["operation"], "rewind_session");

    let archived = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({"type":"archive_session","session_id":"session-1"}),
    )
    .await;
    assert_eq!(archived["archived"], true);

    let restored = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({"type":"restore_session","session_id":"session-1"}),
    )
    .await;
    assert_eq!(restored["archived"], false);
    let loaded_again = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({"type":"load_session","session_id":"session-1"}),
    )
    .await;
    assert_eq!(loaded_again["messages"][0]["text"], "hello");

    let missing = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({"type":"load_session","session_id":"missing"}),
    )
    .await;
    assert_eq!(missing["type"], "operation_error");
    assert_eq!(missing["operation"], "load_session");

    let deleted = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({"type":"delete_session","session_id":"session-1"}),
    )
    .await;
    assert_eq!(deleted["type"], "session_deleted");
    assert_eq!(deleted["session_id"], "session-1");

    task.abort();
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn reconnect_replays_the_complete_in_flight_turn() {
    let path = socket_path();
    let agent_id = AgentId::new("agent-1");
    let bus = Arc::new(InProcessMessageBus::new());
    let store: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store"));
    store
        .save(&StoredSession::new(
            SessionId::new("session-1"),
            "Recovery",
            SessionLifetime::Persistent,
            sylvander_agent::session::SessionMetadata {
                workspace: "/workspace/project".into(),
                name: "Recovery".into(),
                user_id: "unix-client".into(),
            },
            vec![agent_id.clone()],
        ))
        .await
        .expect("save");
    let channel = Arc::new(UnixChannel::new(&path, agent_id.clone()));
    let ui = EmptyUiService {
        chat_bus: Some(bus.clone()),
        ..EmptyUiService::default()
    };
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        bus.clone(),
        store,
        Some(Arc::new(ui)),
        None,
    )));

    let stream = connect(&path).await;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    negotiate(&mut write, &mut lines).await;
    let created = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type":"chat",
            "text":"continue",
            "session_id":"session-1"
        }),
    )
    .await;
    assert_eq!(created["type"], "session_created");
    bus.publish(BusMessage::stream_event(
        SessionId::new("session-1"),
        agent_id.clone(),
        StreamEvent::TextDelta {
            delta: "before ".into(),
        },
    ))
    .await
    .expect("first delta");
    assert!(lines.next_line().await.unwrap().unwrap().contains("before"));
    let concurrent = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({"type":"chat","text":"race","session_id":"session-1"}),
    )
    .await;
    assert_eq!(concurrent["type"], "operation_error");
    assert_eq!(concurrent["operation"], "chat");
    drop(lines);
    drop(write);

    bus.publish(BusMessage::stream_event(
        SessionId::new("session-1"),
        agent_id,
        StreamEvent::TextDelta {
            delta: "after".into(),
        },
    ))
    .await
    .expect("missed delta");

    let stream = connect(&path).await;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    negotiate(&mut write, &mut lines).await;
    let history = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({"type":"reattach_session","session_id":"session-1"}),
    )
    .await;
    assert_eq!(history["type"], "session_history");
    assert_eq!(history["recovery"], true);
    let replayed = [
        lines.next_line().await.unwrap().unwrap(),
        lines.next_line().await.unwrap().unwrap(),
    ]
    .join(" ");
    assert!(replayed.contains("before"));
    assert!(replayed.contains("after"));

    task.abort();
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn terminal_error_reaches_the_client_and_releases_the_session_relay() {
    let path = socket_path();
    let agent_id = AgentId::new("agent-1");
    let bus = Arc::new(InProcessMessageBus::new());
    let store: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store"));
    let feedback_target = sylvander_protocol::FeedbackTarget(format!("sha256:{}", "a".repeat(64)));
    let channel = Arc::new(UnixChannel::new(&path, agent_id.clone()));
    let ui = EmptyUiService {
        chat_bus: Some(bus.clone()),
        feedback_target: Some(feedback_target.clone()),
        ..EmptyUiService::default()
    };
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        bus.clone(),
        store,
        Some(Arc::new(ui)),
        None,
    )));

    let stream = connect(&path).await;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    negotiate(&mut write, &mut lines).await;
    let created = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type":"chat",
            "text":"fail",
            "session_id":"session-1"
        }),
    )
    .await;
    assert_eq!(created["type"], "session_created");

    bus.publish(BusMessage::stream_event(
        SessionId::new("session-1"),
        agent_id,
        StreamEvent::Error {
            message: "provider unavailable".into(),
        },
    ))
    .await
    .expect("publish terminal error");
    let error: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(1), lines.next_line())
            .await
            .expect("error timeout")
            .expect("error read")
            .expect("error event"),
    )
    .expect("error json");
    assert_eq!(error["type"], "error");
    assert_eq!(error["session_id"], "session-1");
    assert_eq!(error["message"], "provider unavailable");
    assert_eq!(error["feedback_target"], feedback_target.0);

    tokio::task::yield_now().await;
    let next = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type":"chat",
            "text":"retry",
            "session_id":"session-1"
        }),
    )
    .await;
    assert_eq!(
        next["type"], "session_created",
        "terminal errors must release the per-session relay"
    );

    task.abort();
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn socket_permissions_and_live_events_are_isolated_between_clients() {
    let path = socket_path();
    let agent_id = AgentId::new("agent-1");
    let bus = Arc::new(InProcessMessageBus::new());
    let store: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store"));
    let channel = Arc::new(UnixChannel::new(&path, agent_id.clone()));
    let ui = EmptyUiService {
        chat_bus: Some(bus.clone()),
        ..EmptyUiService::default()
    };
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        bus.clone(),
        store,
        Some(Arc::new(ui)),
        None,
    )));

    let stream_a = connect(&path).await;
    assert_eq!(
        std::fs::metadata(&path)
            .expect("socket metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the local Agent socket must not be accessible to other OS users"
    );
    let (read_a, mut write_a) = stream_a.into_split();
    let mut lines_a = BufReader::new(read_a).lines();
    negotiate(&mut write_a, &mut lines_a).await;

    let stream_b = connect(&path).await;
    let (read_b, mut write_b) = stream_b.into_split();
    let mut lines_b = BufReader::new(read_b).lines();
    negotiate(&mut write_b, &mut lines_b).await;

    let created_a = send_and_read(
        &mut write_a,
        &mut lines_a,
        serde_json::json!({"type":"chat","text":"a","session_id":"session-a"}),
    )
    .await;
    let created_b = send_and_read(
        &mut write_b,
        &mut lines_b,
        serde_json::json!({"type":"chat","text":"b","session_id":"session-b"}),
    )
    .await;
    assert_eq!(created_a["session_id"], "session-a");
    assert_eq!(created_b["session_id"], "session-b");

    for (session, delta) in [("session-a", "only-a"), ("session-b", "only-b")] {
        bus.publish(BusMessage::stream_event(
            SessionId::new(session),
            agent_id.clone(),
            StreamEvent::TextDelta {
                delta: delta.into(),
            },
        ))
        .await
        .expect("publish isolated event");
    }

    let event_a: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(1), lines_a.next_line())
            .await
            .expect("client A timeout")
            .expect("client A read")
            .expect("client A event"),
    )
    .expect("client A json");
    let event_b: serde_json::Value = serde_json::from_str(
        &tokio::time::timeout(std::time::Duration::from_secs(1), lines_b.next_line())
            .await
            .expect("client B timeout")
            .expect("client B read")
            .expect("client B event"),
    )
    .expect("client B json");
    assert_eq!(event_a["session_id"], "session-a");
    assert_eq!(event_a["delta"], "only-a");
    assert_eq!(event_b["session_id"], "session-b");
    assert_eq!(event_b["delta"], "only-b");

    bus.publish(BusMessage::stream_event(
        SessionId::new("session-a"),
        agent_id,
        StreamEvent::TextDelta {
            delta: "still-only-a".into(),
        },
    ))
    .await
    .expect("publish follow-up");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), lines_b.next_line())
            .await
            .is_err(),
        "client B received an event from client A's session"
    );

    task.abort();
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn typed_plan_resolution_is_forwarded_to_the_agent_bus() {
    let bus = Arc::new(InProcessMessageBus::new());
    let agent_id = AgentId::new("agent-1");
    let mut inbox = bus
        .subscribe(SubscriptionFilter::for_agent(agent_id.clone()))
        .await
        .expect("subscribe");
    let ui = EmptyUiService {
        chat_bus: Some(bus.clone()),
        ..EmptyUiService::default()
    };
    let context = ChannelContext::with_services(
        bus,
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(ui)),
        None,
    );
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::ResolvePlan {
            session_id: "session-1".into(),
            plan_id: "plan-1".into(),
            decision: sylvander_protocol::PlanDecision::Revised {
                steps: vec!["inspect".into(), "verify".into()],
            },
        },
        &context,
        &agent_id,
        &tx,
        &runtime_info(),
    )
    .await;

    let message = inbox.recv().await.expect("agent message");
    assert!(matches!(
        (message.session_id.0.as_str(), message.kind),
        ("session-1",
        MessageKind::System(SystemMessage::ResolvePlan {
            plan_id,
            decision: sylvander_protocol::PlanDecision::Revised { steps },
        })) if plan_id == "plan-1" && steps == ["inspect", "verify"]
    ));
}

#[tokio::test]
async fn approval_decision_is_forwarded_without_transport_interpretation() {
    let bus = Arc::new(InProcessMessageBus::new());
    let agent_id = AgentId::new("agent-1");
    let mut inbox = bus
        .subscribe(SubscriptionFilter::for_agent(agent_id.clone()))
        .await
        .expect("subscribe");
    let ui = EmptyUiService {
        chat_bus: Some(bus.clone()),
        ..EmptyUiService::default()
    };
    let context = ChannelContext::with_services(
        bus,
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(ui)),
        None,
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::Approve {
            session_id: "session-1".into(),
            call_id: "call-1".into(),
            approved: false,
            scope: sylvander_protocol::ApprovalScope::Session,
            reason: Some("unsafe outside workspace".into()),
        },
        &context,
        &agent_id,
        &tx,
        &runtime_info(),
    )
    .await;

    let message = inbox.recv().await.expect("agent message");
    assert!(matches!(
        (message.session_id.0.as_str(), message.kind),
        ("session-1",
        MessageKind::System(SystemMessage::ApproveTool {
            call_id,
            approved: false,
            scope: sylvander_protocol::ApprovalScope::Session,
            reason: Some(reason),
        })) if call_id == "call-1" && reason == "unsafe outside workspace"
    ));
}

#[tokio::test]
async fn task_cancel_preserves_session_scope_on_the_agent_bus() {
    let bus = Arc::new(InProcessMessageBus::new());
    let agent_id = AgentId::new("agent-1");
    let mut inbox = bus
        .subscribe(SubscriptionFilter::for_agent(agent_id.clone()))
        .await
        .expect("subscribe");
    let ui = EmptyUiService {
        chat_bus: Some(bus.clone()),
        ..EmptyUiService::default()
    };
    let context = ChannelContext::with_services(
        bus,
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(ui)),
        None,
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::CancelTask {
            session_id: "session-1".into(),
            task_id: "task-1".into(),
        },
        &context,
        &agent_id,
        &tx,
        &runtime_info(),
    )
    .await;

    let message = inbox.recv().await.expect("agent message");
    assert!(matches!(
        message.kind,
        MessageKind::System(SystemMessage::CancelTask { session_id, task_id })
            if session_id.0 == "session-1" && task_id == "task-1"
    ));
}

#[tokio::test]
async fn chat_forwards_typed_attachments_without_flattening() {
    let bus = Arc::new(InProcessMessageBus::new());
    let mut events = bus
        .subscribe(SubscriptionFilter::all())
        .await
        .expect("subscribe");
    let agent_id = AgentId::new("agent-1");
    let ui = EmptyUiService {
        chat_bus: Some(bus.clone()),
        ..EmptyUiService::default()
    };
    let context = ChannelContext::with_services(
        bus,
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(ui)),
        None,
    );
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::Chat {
            text: "review".into(),
            attachments: vec![sylvander_protocol::MessageAttachment {
                id: "a1".into(),
                kind: sylvander_protocol::AttachmentKind::File,
                name: "src/main.rs".into(),
                mime_type: "text/x-rust".into(),
                content: sylvander_protocol::AttachmentContent::Text {
                    text: "fn main() {}".into(),
                },
                byte_count: 12,
            }],
            session_id: Some("session-1".into()),
            workspace: Some("/repo".into()),
        },
        &context,
        &agent_id,
        &tx,
        &runtime_info(),
    )
    .await;

    let chat = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let message = events.recv().await.expect("bus event");
            if matches!(message.kind, MessageKind::Chat) {
                break message;
            }
        }
    })
    .await
    .expect("chat");
    assert_eq!(chat.attachments.len(), 1);
    assert_eq!(chat.attachments[0].name, "src/main.rs");
}
