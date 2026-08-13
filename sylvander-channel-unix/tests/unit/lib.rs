use super::*;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use sylvander_api::{BusMessage, SystemMessage};
use sylvander_channel::{InProcessMessageBus, MessageBus, SubscriptionFilter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn handle_client_msg(
    msg: ClientMsg,
    ctx: &ChannelContext,
    agent_id: &AgentId,
    tx: &mpsc::UnboundedSender<ServerMsg>,
    _runtime: &sylvander_api::RuntimeUiSnapshot,
) {
    let hub = Arc::new(Mutex::new(RelayHub::default()));
    hub.lock().await.clients.insert(0, tx.clone());
    let boundary = sylvander_api::BoundaryContext::authenticated(
        sylvander_api::AuthenticatedPrincipal::user(
            "unix-client",
            sylvander_api::AuthenticationMethod::UnixPeer,
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
            hub: &hub,
            client_id: 0,
            ui_protocol_version: sylvander_api::UI_PROTOCOL_MAX_VERSION,
        },
    )
    .await;
}

#[derive(Default)]
struct EmptyChannelHost {
    registry_authorizations: AtomicUsize,
    registry_dispatches: AtomicUsize,
    snapshot_dispatches: AtomicUsize,
    allow_registry: bool,
    session_config: Option<sylvander_api::SessionConfigState>,
    chat_bus: Option<Arc<dyn MessageBus>>,
    feedback_target: Option<sylvander_api::FeedbackTarget>,
    compaction: Option<sylvander_api::CompactionReport>,
    rollback_preview: Option<sylvander_api::WorkspaceRollbackPreview>,
    rollback_report: Option<sylvander_api::WorkspaceRollbackReport>,
    allow_delete: bool,
    session_history: Mutex<Option<sylvander_api::UiSessionHistory>>,
}

#[tokio::test]
async fn oversized_frame_is_rejected_before_deserialization() {
    let (mut client, server) = tokio::io::duplex(64);
    let mut reader = FramedRead::new(server, LinesCodec::new_with_max_length(4));
    client.write_all(b"12345\n").await.unwrap();

    assert!(reader.next().await.unwrap().is_err());
}

#[async_trait]
impl sylvander_channel::ChannelHost for EmptyChannelHost {
    async fn authorize_message(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        message: &ClientMsg,
    ) -> Result<(), sylvander_api::BoundaryError> {
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
            return Err(sylvander_api::BoundaryError::forbidden(
                boundary,
                "registry_admin",
            ));
        }
        Ok(())
    }

    async fn submit_chat(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        request: sylvander_channel::ExternalChatRequest,
    ) -> Result<sylvander_channel::SubmittedChat, sylvander_api::BoundaryError> {
        let bus = self
            .chat_bus
            .as_ref()
            .ok_or_else(|| sylvander_api::BoundaryError::forbidden(boundary, "submit_chat"))?;
        let principal = boundary.principal.as_ref().ok_or_else(|| {
            sylvander_api::BoundaryError::unauthenticated(boundary, "submit_chat")
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
            .map_err(|_| sylvander_api::BoundaryError::forbidden(boundary, "submit_chat"))?;
        bus.publish(BusMessage {
            session_id: session_id.clone(),
            sender: sylvander_api::Sender::User(principal.id.0.clone()),
            recipient: sylvander_api::Recipient::Agent(request.agent_id),
            kind: MessageKind::Chat,
            payload: request.text,
            attachments: request.attachments,
            timestamp: 0,
            id: sylvander_api::MessageId::new(),
        })
        .await
        .map_err(|_| sylvander_api::BoundaryError::forbidden(boundary, "submit_chat"))?;
        Ok(sylvander_channel::SubmittedChat {
            session_id,
            feedback_target: self.feedback_target.clone(),
            events,
        })
    }

    async fn submit_control(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        message: ClientMsg,
    ) -> Result<(), sylvander_api::BoundaryError> {
        let bus = self
            .chat_bus
            .as_ref()
            .ok_or_else(|| sylvander_api::BoundaryError::forbidden(boundary, "submit_control"))?;
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
                return Err(sylvander_api::BoundaryError::forbidden(
                    boundary,
                    "submit_control",
                ));
            }
        };
        bus.publish(BusMessage {
            session_id,
            sender: sylvander_api::Sender::System,
            recipient: sylvander_api::Recipient::Agent(AgentId::new("agent-1")),
            kind: MessageKind::System(system),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: 0,
            id: sylvander_api::MessageId::new(),
        })
        .await
        .map_err(|_| sylvander_api::BoundaryError::forbidden(boundary, "submit_control"))
    }

    async fn delete_session(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        _: &SessionId,
    ) -> Result<(), sylvander_api::BoundaryError> {
        if self.allow_delete {
            Ok(())
        } else {
            Err(sylvander_api::BoundaryError::forbidden(
                boundary,
                "delete_session",
            ))
        }
    }

    async fn load_session(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<sylvander_api::UiSessionHistory, sylvander_api::BoundaryError> {
        self.session_history
            .lock()
            .await
            .clone()
            .filter(|history| history.session.id == session_id.0)
            .ok_or_else(|| sylvander_api::BoundaryError::forbidden(boundary, "load_session"))
    }

    async fn rename_session(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        session_id: &SessionId,
        label: String,
    ) -> Result<(), sylvander_api::BoundaryError> {
        let mut history = self.session_history.lock().await;
        let history = history
            .as_mut()
            .filter(|history| history.session.id == session_id.0)
            .ok_or_else(|| sylvander_api::BoundaryError::forbidden(boundary, "rename_session"))?;
        history.session.label = label;
        Ok(())
    }

    async fn archive_session(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<(), sylvander_api::BoundaryError> {
        self.load_session(boundary, session_id).await.map(|_| ())
    }

    async fn restore_session(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<(), sylvander_api::BoundaryError> {
        self.load_session(boundary, session_id).await.map(|_| ())
    }

    async fn fork_session(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        session_id: &SessionId,
        completed_turns: Option<usize>,
        checkpoint: bool,
    ) -> Result<sylvander_api::UiSessionHistory, sylvander_api::BoundaryError> {
        let mut history = self.load_session(boundary, session_id).await?;
        if let Some(turns) = completed_turns {
            let end = history
                .messages
                .iter()
                .enumerate()
                .filter(|(_, message)| message.role == "assistant")
                .nth(turns.saturating_sub(1))
                .map(|(index, _)| index + 1)
                .ok_or_else(|| sylvander_api::BoundaryError {
                    code: sylvander_api::BoundaryErrorCode::InvalidScope,
                    operation: "fork_session".into(),
                    request_id: boundary.request_id.clone(),
                    message: format!("completed turn {turns} does not exist"),
                    retry_after_ms: None,
                })?;
            history.messages.truncate(end);
            history.session.label = format!("{} (rewind {turns})", history.session.label);
        } else if checkpoint {
            history.session.label.push_str(" (checkpoint)");
        } else {
            history.session.label.push_str(" (fork)");
        }
        history.session.id = uuid::Uuid::new_v4().to_string();
        history.iterations = 0;
        history.input_tokens = 0;
        history.output_tokens = 0;
        history.cost_nano_usd = Some(0);
        Ok(history)
    }

    async fn discover_agents(
        &self,
        _boundary: &sylvander_api::BoundaryContext,
    ) -> Result<Vec<sylvander_api::AgentDescriptor>, sylvander_api::BoundaryError> {
        Ok(Vec::new())
    }

    async fn runtime_snapshot(
        &self,
        _: &sylvander_api::BoundaryContext,
        _: &AgentId,
        _: Option<&SessionId>,
    ) -> Result<sylvander_api::RuntimeUiSnapshot, sylvander_api::BoundaryError> {
        self.snapshot_dispatches.fetch_add(1, Ordering::Relaxed);
        Ok(runtime_info())
    }

    async fn create_session(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        _request: sylvander_api::SessionCreateRequest,
    ) -> Result<sylvander_api::SessionConfigState, sylvander_api::BoundaryError> {
        Err(sylvander_api::BoundaryError::forbidden(
            boundary,
            "create_session",
        ))
    }

    async fn session_config(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_api::SessionConfigState, sylvander_api::BoundaryError> {
        self.session_config
            .clone()
            .ok_or_else(|| sylvander_api::BoundaryError::forbidden(boundary, "get_session_config"))
    }

    async fn update_session_config(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        _request: sylvander_api::SessionConfigUpdateRequest,
    ) -> Result<sylvander_api::SessionConfigState, sylvander_api::BoundaryError> {
        Err(sylvander_api::BoundaryError::forbidden(
            boundary,
            "update_session_config",
        ))
    }

    async fn submit_feedback(
        &self,
        _boundary: &sylvander_api::BoundaryContext,
        _feedback: sylvander_api::RunFeedback,
    ) -> Result<String, sylvander_api::BoundaryError> {
        Ok("feedback-1".into())
    }

    async fn memory_confirmation(
        &self,
        _boundary: &sylvander_api::BoundaryContext,
        request: sylvander_api::MemoryConfirmationRequest,
    ) -> sylvander_api::MemoryConfirmationResponse {
        match request {
            sylvander_api::MemoryConfirmationRequest::List { session_id, .. } => {
                sylvander_api::MemoryConfirmationResponse::Pending {
                    version: sylvander_api::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                    session_id,
                    confirmations: vec![sylvander_api::PendingMemoryConfirmation {
                        candidate_id: "candidate-1".into(),
                        expected_revision: 2,
                        scope: sylvander_api::MemoryConfirmationScope::UserProfile,
                        summary: "prefers concise answers".into(),
                    }],
                }
            }
            sylvander_api::MemoryConfirmationRequest::Decide {
                session_id,
                candidate_id,
                decision,
                ..
            } => sylvander_api::MemoryConfirmationResponse::Recorded {
                version: sylvander_api::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                session_id,
                candidate_id,
                decision,
            },
        }
    }

    fn identity_binding_capabilities(&self) -> sylvander_api::IdentityBindingCapabilities {
        sylvander_api::IdentityBindingCapabilities::current()
    }

    async fn identity_binding(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        identity: sylvander_channel::AuthenticatedTransportIdentity,
        _request: sylvander_api::IdentityBindingRequest,
    ) -> sylvander_api::IdentityBindingResponse {
        let (transport, instance, principal) = identity.into_parts();
        assert_eq!(transport, boundary.transport);
        assert_eq!(instance, boundary.channel_instance_id);
        assert_eq!(
            principal,
            boundary.principal.as_ref().expect("principal").id.0
        );
        sylvander_api::IdentityBindingResponse::Resolved {
            version: sylvander_api::IDENTITY_BINDING_PROTOCOL_VERSION,
            binding: sylvander_api::IdentityBindingView {
                user_id: sylvander_api::UserId::new("stable-user"),
                revision: 7,
                linked_at_unix_secs: 11,
            },
        }
    }

    async fn compact_session(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_api::CompactionReport, sylvander_api::BoundaryError> {
        self.compaction
            .clone()
            .ok_or_else(|| sylvander_api::BoundaryError::forbidden(boundary, "compact_session"))
    }

    async fn preview_workspace_rollback(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_api::WorkspaceRollbackPreview, sylvander_api::BoundaryError> {
        self.rollback_preview.clone().ok_or_else(|| {
            sylvander_api::BoundaryError::forbidden(boundary, "preview_workspace_rollback")
        })
    }

    async fn rollback_workspace(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        _session_id: &SessionId,
        _expected_turn_id: &str,
    ) -> Result<sylvander_api::WorkspaceRollbackReport, sylvander_api::BoundaryError> {
        self.rollback_report
            .clone()
            .ok_or_else(|| sylvander_api::BoundaryError::forbidden(boundary, "rollback_workspace"))
    }

    async fn registry_admin(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        request: sylvander_api::RegistryAdminRequest,
    ) -> sylvander_api::RegistryAdminResponse {
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
            sylvander_api::RegistryAdminRequest::InspectProviderRevision {
                provider_id,
                revision,
            } => sylvander_api::RegistryAdminResult::ProviderRevisionInspected {
                revision: sylvander_api::ProviderRevisionView {
                    definition: sylvander_api::RedactedProviderDefinition {
                        provider_id,
                        revision,
                        kind: "mock".into(),
                        features: BTreeSet::new(),
                        base_url_sha256: "base-digest".into(),
                        credential_binding_id_sha256: "binding-digest".into(),
                    },
                    digest_sha256: "definition-digest".into(),
                    created_at_unix_secs: 7,
                    active: true,
                },
            },
            sylvander_api::RegistryAdminRequest::CreateCredentialBinding { .. } => {
                sylvander_api::RegistryAdminResult::CredentialBindingCreated {
                    generation: sylvander_api::CredentialGenerationView {
                        binding_id_sha256: "binding-id-digest".into(),
                        generation: 1,
                        reference_kind: sylvander_api::CredentialReferenceKind::Environment,
                        reference_configured: true,
                        reference_digest_sha256: "reference-digest".into(),
                        created_at_unix_secs: 7,
                        active: true,
                    },
                }
            }
            _ => unreachable!(),
        };
        sylvander_api::RegistryAdminResponse::Success {
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

fn runtime_info() -> sylvander_api::RuntimeUiSnapshot {
    sylvander_api::RuntimeUiSnapshot {
        agent_id: AgentId::new("agent-1"),
        model: sylvander_api::ModelSelection {
            provider_id: "test".into(),
            model_id: "test-model".into(),
        },
        reasoning_effort: sylvander_api::ReasoningEffort::Off,
        models: vec![sylvander_api::ModelDescriptor {
            id: "test-model".into(),
            provider: "test".into(),
            capabilities: 0b101,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_api::ReasoningEffort::Off],
            lifecycle: sylvander_api::ModelLifecycle::Active,
            pricing: None,
        }],
        permissions: sylvander_api::PermissionProfile::default(),
        capabilities: 0b101,
        approval_enabled: true,
        max_request_bytes: 1024,
        platform: sylvander_api::PlatformSnapshot::default(),
    }
}

fn private_session_config(
    session_id: &str,
    prompt: &str,
    digest: &str,
) -> sylvander_api::SessionConfigState {
    use sylvander_api::{
        PromptLayerDigest, PromptLayerKind, PromptManifest, SessionConfigProvenance,
        SessionConfigSource, SessionConfigSourceKind, SessionEffectiveConfig,
    };
    let source = SessionConfigSource {
        kind: SessionConfigSourceKind::SessionOverride,
        reference: Some("session".into()),
    };
    sylvander_api::SessionConfigState {
        session_id: SessionId::new(session_id),
        revision: 2,
        overrides: sylvander_api::SessionConfigOverrides {
            system_prompt: Some(prompt.into()),
            ..sylvander_api::SessionConfigOverrides::default()
        },
        effective: SessionEffectiveConfig {
            agent_id: AgentId::new("agent-1"),
            agent_revision: 1,
            provider_id: "test".into(),
            provider_revision: 1,
            model_id: "test-model".into(),
            model_revision: 1,
            reasoning_effort: sylvander_api::ReasoningEffort::Off,
            permissions: sylvander_api::PermissionProfile::default(),
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
                "min_version":sylvander_api::UI_PROTOCOL_VERSION,
                "max_version":sylvander_api::UI_PROTOCOL_VERSION,
                "capabilities":[]
            }
        }),
    )
    .await;
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(
        welcome["protocol"]["version"],
        sylvander_api::UI_PROTOCOL_VERSION
    );
}

#[tokio::test]
async fn runtime_info_reports_server_truth() {
    let bus = Arc::new(InProcessMessageBus::new());
    let context = ChannelContext::with_services(
        bus,
        Some("unix".into()),
        Some(Arc::new(EmptyChannelHost::default())),
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::GetRuntimeInfo {
            agent_id: AgentId::new("agent-1"),
        },
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
            snapshot: sylvander_api::RuntimeUiSnapshot {
            model,
            reasoning_effort: sylvander_api::ReasoningEffort::Off,
            models,
            permissions: sylvander_api::PermissionProfile {
                file_access: sylvander_api::FileAccess::WorkspaceWrite,
                network_access: sylvander_api::NetworkAccess::Denied,
                approval_policy: sylvander_api::ApprovalPolicy::Allow,
            },
            capabilities: 0b101,
            approval_enabled: true,
            max_request_bytes: 1024,
            ..
            }
        } if model.provider_id == "test"
            && model.model_id == "test-model"
            && models.len() == 1
    ));
}

#[tokio::test]
async fn runtime_info_queries_the_channel_host_for_each_request() {
    let bus = Arc::new(InProcessMessageBus::new());
    let host = Arc::new(EmptyChannelHost::default());
    let context = ChannelContext::with_services(bus, Some("unix".into()), Some(host.clone()), None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    for _ in 0..2 {
        handle_client_msg(
            ClientMsg::GetRuntimeInfo {
                agent_id: AgentId::new("agent-1"),
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::RuntimeInfo { .. })
        ));
    }
    assert_eq!(host.snapshot_dispatches.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn agent_discovery_is_served_through_the_channel_host_boundary() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Some("unix".into()),
        Some(Arc::new(EmptyChannelHost::default())),
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
            feedback: sylvander_api::RunFeedback {
                target: sylvander_api::FeedbackTarget("sha256:target".into()),
                rating: sylvander_api::FeedbackRating::Positive,
                note: None,
                correction: None,
                tags: Vec::new(),
                task_result: None,
                artifacts: Vec::new(),
                validations: Vec::new(),
                privacy_class: sylvander_api::FeedbackPrivacyClass::Private,
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
        Some("unix".into()),
        Some(Arc::new(EmptyChannelHost::default())),
        None,
    );
    let boundary = sylvander_api::BoundaryContext::authenticated(
        sylvander_api::AuthenticatedPrincipal::user(
            "unix:local:uid:501",
            sylvander_api::AuthenticationMethod::UnixPeer,
        ),
        "local",
        "unix",
        "identity-request",
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg_for_client(
        ClientMsg::IdentityBinding {
            request: Arc::new(sylvander_api::IdentityBindingRequest {
                version: sylvander_api::IDENTITY_BINDING_PROTOCOL_VERSION,
                action: sylvander_api::IdentityBindingAction::Resolve {},
            }),
        },
        ClientHandler {
            boundary: &boundary,
            ctx: &context,
            agent_id: &AgentId::new("agent-1"),
            tx: &tx,
            hub: &Arc::new(Mutex::new(RelayHub::default())),
            client_id: 1,
            ui_protocol_version: sylvander_api::UI_PROTOCOL_MAX_VERSION,
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
                sylvander_api::IdentityBindingResponse::Resolved {
                    binding,
                    ..
                } if binding.user_id == sylvander_api::UserId::new("stable-user")
                    && binding.revision == 7
            )
    ));
}

#[tokio::test]
async fn agent_admin_without_channel_host_returns_content_free_error() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Some("unix".into()),
        None,
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::AgentAdmin {
            request: sylvander_api::AgentAdminRequest::InspectRevision {
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
            response: sylvander_api::AgentAdminResponse::Error {
                error: sylvander_api::AgentAdminError {
                    code: sylvander_api::AgentAdminErrorCode::Unauthorized,
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
async fn registry_admin_without_channel_host_returns_content_free_error() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Some("unix".into()),
        None,
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::RegistryAdmin {
            request: sylvander_api::RegistryAdminRequest::InspectProviderRevision {
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
            response: sylvander_api::RegistryAdminResponse::Error {
                error: sylvander_api::RegistryAdminError {
                    code: sylvander_api::RegistryAdminErrorCode::Unauthorized,
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
    principal: sylvander_api::AuthenticatedPrincipal,
    request: ClientMsg,
) -> ServerMsg {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Some("unix".into()),
        Some(Arc::new(EmptyChannelHost::default())),
        None,
    );
    let boundary =
        sylvander_api::BoundaryContext::authenticated(principal, "unix-test", "unix", "request-1");
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg_for_client(
        request,
        ClientHandler {
            boundary: &boundary,
            ctx: &context,
            agent_id: &AgentId::new("agent-1"),
            tx: &tx,
            hub: &Arc::new(Mutex::new(RelayHub::default())),
            client_id: 1,
            ui_protocol_version: sylvander_api::UI_PROTOCOL_MAX_VERSION,
        },
    )
    .await;
    rx.recv().await.expect("registry transport response")
}

#[tokio::test]
async fn registry_admin_round_trip_preserves_success_response() {
    let mut principal = sylvander_api::AuthenticatedPrincipal::user(
        "admin",
        sylvander_api::AuthenticationMethod::UnixPeer,
    );
    principal.roles.push("admin".into());
    let response = dispatch_client_message_as(principal, inspect_registry_request()).await;
    let wire = serde_json::to_string(&response).expect("encode registry response");
    let decoded: ServerMsg = serde_json::from_str(&wire).expect("decode registry response");

    assert!(matches!(
        decoded,
        ServerMsg::RegistryAdmin {
            response: sylvander_api::RegistryAdminResponse::Success { result }
        } if matches!(
            result.as_ref(),
            sylvander_api::RegistryAdminResult::ProviderRevisionInspected { revision }
                if revision.definition.provider_id == "provider-a"
                    && revision.definition.revision == 9
        )
    ));
}

#[tokio::test]
async fn registry_admin_non_administrator_is_rejected_before_dispatch() {
    let principal = sylvander_api::AuthenticatedPrincipal::user(
        "reader",
        sylvander_api::AuthenticationMethod::UnixPeer,
    );
    assert!(matches!(
        dispatch_client_message_as(principal, inspect_registry_request()).await,
        ServerMsg::BoundaryDenied { error }
            if error.code == sylvander_api::BoundaryErrorCode::Forbidden
                && error.operation == "registry_admin"
    ));
}

#[test]
fn server_advertises_administration_capabilities() {
    let capabilities = ui_protocol_capabilities();
    assert!(
        capabilities
            .iter()
            .any(|item| item == sylvander_api::IDENTITY_BINDING_CAPABILITY)
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
    let service = Arc::new(EmptyChannelHost {
        allow_registry: true,
        ..EmptyChannelHost::default()
    });
    let channel = Arc::new(UnixChannel::new(&path, "agent-1"));
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Some("unix".into()),
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
            "min_version": sylvander_api::UI_PROTOCOL_VERSION - 1,
            "max_version": sylvander_api::UI_PROTOCOL_VERSION - 1,
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
                "min_version": sylvander_api::UI_PROTOCOL_VERSION,
                "max_version": sylvander_api::UI_PROTOCOL_VERSION,
                "capabilities": []
            }
        }),
    )
    .await;
    assert_eq!(
        welcome["protocol"]["version"],
        sylvander_api::UI_PROTOCOL_VERSION
    );
    let duplicate = send_and_read(
        &mut write,
        &mut lines,
        serde_json::json!({
            "type": "hello",
            "protocol": {
                "client_name": "current-client",
                "min_version": sylvander_api::UI_PROTOCOL_VERSION,
                "max_version": sylvander_api::UI_PROTOCOL_VERSION,
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
        Some("unix".into()),
        Some(Arc::new(EmptyChannelHost::default())),
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
                "version": sylvander_api::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
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
                "version": sylvander_api::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
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
    let service = Arc::new(EmptyChannelHost {
        session_config: Some(private_session_config("session-secret", SENTINEL, DIGEST)),
        ..EmptyChannelHost::default()
    });
    let channel = Arc::new(UnixChannel::new(&path, "agent-1"));
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Some("unix".into()),
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
    let mut principal = sylvander_api::AuthenticatedPrincipal::user(
        "admin",
        sylvander_api::AuthenticationMethod::UnixPeer,
    );
    principal.roles.push("admin".into());

    let response = dispatch_client_message_as(principal, request).await;
    let wire = serde_json::to_string(&response).expect("encode credential response");
    assert!(!wire.contains(binding_id));
    assert!(!wire.contains(locator));
    assert!(matches!(
        response,
        ServerMsg::RegistryAdmin {
            response: sylvander_api::RegistryAdminResponse::Success { result }
        } if matches!(
            result.as_ref(),
            sylvander_api::RegistryAdminResult::CredentialBindingCreated { generation }
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
        Some("unix".into()),
        Some(Arc::new(EmptyChannelHost::default())),
        None,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::SelectModel {
            session_id: None,
            model: sylvander_api::ModelSelection {
                provider_id: "test".into(),
                model_id: "thinking-model".into(),
            },
            reasoning_effort: sylvander_api::ReasoningEffort::Medium,
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
            profile: sylvander_api::PermissionProfile {
                file_access: sylvander_api::FileAccess::ReadOnly,
                network_access: sylvander_api::NetworkAccess::Denied,
                approval_policy: sylvander_api::ApprovalPolicy::Deny,
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
    let ui = EmptyChannelHost {
        rollback_preview: Some(sylvander_api::WorkspaceRollbackPreview {
            turn_id: "turn-1".into(),
            files: vec!["file.txt".into()],
        }),
        rollback_report: Some(sylvander_api::WorkspaceRollbackReport {
            turn_id: "turn-1".into(),
            restored: vec!["file.txt".into()],
        }),
        ..EmptyChannelHost::default()
    };
    let context = ChannelContext::with_services(bus, Some("unix".into()), Some(Arc::new(ui)), None);
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
    let history = sylvander_api::UiSessionHistory {
        session: sylvander_api::UiSessionInfo {
            id: "session-1".into(),
            label: "Original".into(),
            workspace: "/workspace/project".into(),
            last_seen_secs: 0,
            archived: false,
        },
        messages: vec![
            sylvander_api::UiHistoryMessage {
                role: "user".into(),
                text: "hello".into(),
            },
            sylvander_api::UiHistoryMessage {
                role: "assistant".into(),
                text: "answer one".into(),
            },
            sylvander_api::UiHistoryMessage {
                role: "user".into(),
                text: "question two".into(),
            },
            sylvander_api::UiHistoryMessage {
                role: "assistant".into(),
                text: "answer two".into(),
            },
        ],
        iterations: 1,
        input_tokens: 120,
        output_tokens: 30,
        cost_nano_usd: Some(45_000),
    };

    let channel = Arc::new(UnixChannel::new(&path, agent_id));
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Some("unix".into()),
        Some(Arc::new(EmptyChannelHost {
            allow_delete: true,
            session_history: Mutex::new(Some(history)),
            ..EmptyChannelHost::default()
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
    assert_eq!(invalid_rewind["operation"], "fork_session");

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
    let channel = Arc::new(UnixChannel::new(&path, agent_id.clone()));
    let ui = EmptyChannelHost {
        chat_bus: Some(bus.clone()),
        session_history: Mutex::new(Some(sylvander_api::UiSessionHistory {
            session: sylvander_api::UiSessionInfo {
                id: "session-1".into(),
                label: "Recovery".into(),
                workspace: "/workspace/project".into(),
                last_seen_secs: 0,
                archived: false,
            },
            messages: Vec::new(),
            iterations: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_nano_usd: Some(0),
        })),
        ..EmptyChannelHost::default()
    };
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        bus.clone(),
        Some("unix".into()),
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
        StreamEvent::TurnStarted {
            turn_id: "turn-1".into(),
        },
    ))
    .await
    .expect("turn start");
    let started: serde_json::Value = serde_json::from_str(
        &lines
            .next_line()
            .await
            .unwrap()
            .expect("turn start reaches the first client"),
    )
    .expect("turn start JSON");
    assert_eq!(started["type"], "turn_started");
    assert_eq!(started["turn_id"], "turn-1");
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
        lines.next_line().await.unwrap().unwrap(),
    ]
    .join(" ");
    assert!(replayed.contains("turn_started"));
    assert!(replayed.contains("turn-1"));
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
    let feedback_target = sylvander_api::FeedbackTarget(format!("sha256:{}", "a".repeat(64)));
    let channel = Arc::new(UnixChannel::new(&path, agent_id.clone()));
    let ui = EmptyChannelHost {
        chat_bus: Some(bus.clone()),
        feedback_target: Some(feedback_target.clone()),
        ..EmptyChannelHost::default()
    };
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        bus.clone(),
        Some("unix".into()),
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
    let channel = Arc::new(UnixChannel::new(&path, agent_id.clone()));
    let ui = EmptyChannelHost {
        chat_bus: Some(bus.clone()),
        ..EmptyChannelHost::default()
    };
    let task = tokio::spawn(channel.run(ChannelContext::with_services(
        bus.clone(),
        Some("unix".into()),
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
    let ui = EmptyChannelHost {
        chat_bus: Some(bus.clone()),
        ..EmptyChannelHost::default()
    };
    let context = ChannelContext::with_services(bus, Some("unix".into()), Some(Arc::new(ui)), None);
    let (tx, _rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::ResolvePlan {
            session_id: "session-1".into(),
            plan_id: "plan-1".into(),
            decision: sylvander_api::PlanDecision::Revised {
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
            decision: sylvander_api::PlanDecision::Revised { steps },
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
    let ui = EmptyChannelHost {
        chat_bus: Some(bus.clone()),
        ..EmptyChannelHost::default()
    };
    let context = ChannelContext::with_services(bus, Some("unix".into()), Some(Arc::new(ui)), None);
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::Approve {
            session_id: "session-1".into(),
            call_id: "call-1".into(),
            approved: false,
            scope: sylvander_api::ApprovalScope::Session,
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
            scope: sylvander_api::ApprovalScope::Session,
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
    let ui = EmptyChannelHost {
        chat_bus: Some(bus.clone()),
        ..EmptyChannelHost::default()
    };
    let context = ChannelContext::with_services(bus, Some("unix".into()), Some(Arc::new(ui)), None);
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
    let ui = EmptyChannelHost {
        chat_bus: Some(bus.clone()),
        ..EmptyChannelHost::default()
    };
    let context = ChannelContext::with_services(bus, Some("unix".into()), Some(Arc::new(ui)), None);
    let (tx, _rx) = mpsc::unbounded_channel();
    handle_client_msg(
        ClientMsg::Chat {
            text: "review".into(),
            attachments: vec![sylvander_api::MessageAttachment {
                id: "a1".into(),
                kind: sylvander_api::AttachmentKind::File,
                name: "src/main.rs".into(),
                mime_type: "text/x-rust".into(),
                content: sylvander_api::AttachmentContent::Text {
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
