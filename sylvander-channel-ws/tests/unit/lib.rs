use super::*;
use sylvander_agent::bus::InProcessMessageBus;
use sylvander_agent::session_store::{SessionStore, SqliteSessionStore};
use sylvander_channel::UiService;

struct DenyAgentAccess;

struct SessionConfigUi {
    states: Mutex<HashMap<String, sylvander_protocol::SessionConfigState>>,
}

struct CredentialRegistryUi {
    received: Mutex<Option<sylvander_protocol::RegistryAdminRequest>>,
}

#[async_trait]
impl UiService for CredentialRegistryUi {
    async fn authorize_message(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        message: &ClientMsg,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        if matches!(message, ClientMsg::IdentityBinding { .. }) {
            return Ok(());
        }
        if matches!(message, ClientMsg::RegistryAdmin { .. })
            && boundary
                .principal
                .as_ref()
                .is_some_and(|principal| principal.has_role("admin"))
        {
            Ok(())
        } else {
            Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                "registry_admin",
            ))
        }
    }

    async fn discover_agents(
        &self,
        _: &sylvander_protocol::BoundaryContext,
    ) -> Result<Vec<sylvander_protocol::AgentDescriptor>, sylvander_protocol::BoundaryError> {
        unreachable!()
    }

    async fn create_session(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: sylvander_protocol::SessionCreateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        unreachable!()
    }

    async fn session_config(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: &SessionId,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        unreachable!()
    }

    async fn update_session_config(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: sylvander_protocol::SessionConfigUpdateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        unreachable!()
    }

    async fn submit_feedback(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: sylvander_protocol::RunFeedback,
    ) -> Result<String, sylvander_protocol::BoundaryError> {
        unreachable!()
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
                user_id: sylvander_protocol::UserId::new("stable-ws-user"),
                revision: 3,
                linked_at_unix_secs: 5,
            },
        }
    }

    async fn registry_admin(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        request: sylvander_protocol::RegistryAdminRequest,
    ) -> sylvander_protocol::RegistryAdminResponse {
        *self.received.lock().await = Some(request);
        sylvander_protocol::RegistryAdminResponse::Success {
            result: Box::new(
                sylvander_protocol::RegistryAdminResult::CredentialBindingCreated {
                    generation: sylvander_protocol::CredentialGenerationView {
                        binding_id_sha256: "binding-digest".into(),
                        generation: 1,
                        reference_kind: sylvander_protocol::CredentialReferenceKind::Environment,
                        reference_configured: true,
                        reference_digest_sha256: "reference-digest".into(),
                        created_at_unix_secs: 7,
                        active: true,
                    },
                },
            ),
        }
    }
}

fn config_state(id: &str) -> sylvander_protocol::SessionConfigState {
    use sylvander_protocol::{
        SessionConfigProvenance, SessionConfigSource, SessionConfigSourceKind,
        SessionEffectiveConfig,
    };
    let source = SessionConfigSource {
        kind: SessionConfigSourceKind::AgentDefault,
        reference: Some("agent-1".into()),
    };
    sylvander_protocol::SessionConfigState {
        session_id: SessionId::new(id),
        revision: 1,
        overrides: sylvander_protocol::SessionConfigOverrides::default(),
        effective: SessionEffectiveConfig {
            agent_id: AgentId::new("agent-1"),
            agent_revision: 1,
            provider_id: "test".into(),
            provider_revision: None,
            model_id: "default-model".into(),
            model_revision: None,
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
            permissions: sylvander_protocol::PermissionProfile::default(),
            prompt_profile: None,
            system_prompt_sha256: "digest".into(),
            prompt_manifest: None,
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

#[async_trait]
impl UiService for SessionConfigUi {
    async fn authorize_message(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        message: &ClientMsg,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        if matches!(message, ClientMsg::RegistryAdmin { .. })
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

    async fn discover_agents(
        &self,
        _: &sylvander_protocol::BoundaryContext,
    ) -> Result<Vec<sylvander_protocol::AgentDescriptor>, sylvander_protocol::BoundaryError> {
        let model = |provider: &str, id: &str| sylvander_protocol::ModelDescriptor {
            id: id.into(),
            provider: provider.into(),
            capabilities: 0,
            capability_names: Vec::new(),
            reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
            lifecycle: sylvander_protocol::ModelLifecycle::Active,
            pricing: None,
        };
        Ok(vec![sylvander_protocol::AgentDescriptor {
            id: AgentId::new("agent-1"),
            revision: 1,
            name: "Agent".into(),
            provider_id: "test".into(),
            default_model_id: "default-model".into(),
            models: vec![
                model("test", "default-model"),
                model("test", "thinking-model"),
                model("provider-a", "shared"),
                model("provider-b", "shared"),
            ],
            default_prompt_profile: None,
            agent_workspace: None,
        }])
    }

    async fn create_session(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: sylvander_protocol::SessionCreateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        unreachable!()
    }

    async fn session_config(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        session_id: &SessionId,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        self.states
            .lock()
            .await
            .get(&session_id.0)
            .cloned()
            .ok_or_else(|| {
                sylvander_protocol::BoundaryError::forbidden(boundary, "get_session_config")
            })
    }

    async fn update_session_config(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: sylvander_protocol::SessionConfigUpdateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        let mut states = self.states.lock().await;
        let state = states.get_mut(&request.session_id.0).ok_or_else(|| {
            sylvander_protocol::BoundaryError::forbidden(boundary, "update_session_config")
        })?;
        assert_eq!(request.expected_revision, state.revision);
        state.revision += 1;
        state.overrides = request.overrides;
        if let Some(model) = &state.overrides.model {
            state.effective.provider_id = model.provider_id.clone();
            state.effective.model_id = model.model_id.clone();
        }
        if let Some(effort) = state.overrides.reasoning_effort {
            state.effective.reasoning_effort = effort;
        }
        if let Some(profile) = &state.overrides.permissions {
            state.effective.permissions = profile.clone();
        }
        Ok(state.clone())
    }

    async fn submit_feedback(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: sylvander_protocol::RunFeedback,
    ) -> Result<String, sylvander_protocol::BoundaryError> {
        unreachable!()
    }

    async fn agent_admin(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        request: sylvander_protocol::AgentAdminRequest,
    ) -> sylvander_protocol::AgentAdminResponse {
        let sylvander_protocol::AgentAdminRequest::ActivateRevision {
            agent_id, revision, ..
        } = request
        else {
            unreachable!()
        };
        sylvander_protocol::AgentAdminResponse::Success {
            result: Box::new(sylvander_protocol::AgentAdminResult::RevisionActivated {
                agent_id,
                active_revision: revision,
            }),
        }
    }

    async fn registry_admin(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        request: sylvander_protocol::RegistryAdminRequest,
    ) -> sylvander_protocol::RegistryAdminResponse {
        assert!(
            boundary
                .principal
                .as_ref()
                .is_some_and(|principal| principal.has_role("admin")),
            "non-administrator reached registry dispatch"
        );
        let sylvander_protocol::RegistryAdminRequest::InspectProviderRevision {
            provider_id,
            revision,
        } = request
        else {
            unreachable!()
        };
        sylvander_protocol::RegistryAdminResponse::Success {
            result: Box::new(
                sylvander_protocol::RegistryAdminResult::ProviderRevisionInspected {
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
            ),
        }
    }
}

#[tokio::test]
async fn welcome_declares_administration_and_credential_lifecycle_capabilities() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        None,
        None,
    );
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "client",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::Hello {
            protocol: sylvander_protocol::UiProtocolHello {
                client_name: "test-client".into(),
                min_version: sylvander_protocol::UI_PROTOCOL_MIN_VERSION,
                max_version: sylvander_protocol::UI_PROTOCOL_MAX_VERSION,
                capabilities: Vec::new(),
            },
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &principal,
        "websocket-test",
    )
    .await;

    let ServerMsg::Welcome { protocol } = rx.recv().await.expect("welcome response") else {
        panic!("hello must receive welcome")
    };
    for capability in [
        "agent_administration",
        "registry_administration",
        "credential_registry_lifecycle",
        "provider_model_registry_lifecycle",
        sylvander_protocol::IDENTITY_BINDING_CAPABILITY,
    ] {
        assert!(
            protocol.capabilities.iter().any(|item| item == capability),
            "missing {capability} capability"
        );
    }
}

fn hello(version: u16) -> ClientMsg {
    ClientMsg::Hello {
        protocol: sylvander_protocol::UiProtocolHello {
            client_name: "test-client".into(),
            min_version: version,
            max_version: version,
            capabilities: Vec::new(),
        },
    }
}

#[tokio::test]
async fn protocol_session_requires_one_leading_hello_and_filters_capabilities() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        None,
        None,
    );
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "client",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut selected = None;

    assert!(
        !handle_protocol_message(
            ClientMsg::Ping,
            &mut selected,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &principal,
            "websocket-test",
        )
        .await
    );
    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::ProtocolError { error }) if error.code == "handshake_required"
    ));

    assert!(
        handle_protocol_message(
            hello(1),
            &mut selected,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &principal,
            "websocket-test"
        )
        .await
    );
    let Some(ServerMsg::Welcome { protocol }) = rx.recv().await else {
        panic!("expected v1 Welcome")
    };
    assert!(
        !protocol
            .capabilities
            .iter()
            .any(|item| item.contains("administration"))
    );
    assert!(
        !protocol
            .capabilities
            .iter()
            .any(|item| item == "provider_model_registry_lifecycle")
    );
    selected = None;

    assert!(
        handle_protocol_message(
            hello(2),
            &mut selected,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &principal,
            "websocket-test",
        )
        .await
    );
    let Some(ServerMsg::Welcome { protocol }) = rx.recv().await else {
        panic!("expected Welcome")
    };
    assert!(
        protocol
            .capabilities
            .iter()
            .any(|item| item == "registry_administration")
    );
    assert!(
        !protocol
            .capabilities
            .iter()
            .any(|item| item == "credential_registry_lifecycle")
    );
    assert!(
        !protocol
            .capabilities
            .iter()
            .any(|item| item == "provider_model_registry_lifecycle")
    );

    assert!(
        handle_protocol_message(
            hello(2),
            &mut selected,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &principal,
            "websocket-test",
        )
        .await
    );
    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::ProtocolError { error }) if error.code == "duplicate_handshake"
    ));
}

#[tokio::test]
async fn credential_mutation_requires_v3_before_authorization_and_dispatch() {
    let ui = Arc::new(CredentialRegistryUi {
        received: Mutex::new(None),
    });
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(ui.clone()),
        None,
    );
    let mut principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "admin",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    principal.roles.push("admin".into());
    let request = || ClientMsg::RegistryAdmin {
        request: sylvander_protocol::RegistryAdminRequest::CreateCredentialBinding {
            binding_id: "private".into(),
            reference: sylvander_protocol::CredentialSecretReferenceDraft::Environment {
                name: "PRIVATE_TOKEN".into(),
            },
        },
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut selected = None;

    assert!(
        handle_protocol_message(
            hello(2),
            &mut selected,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &principal,
            "ws"
        )
        .await
    );
    let _ = rx.recv().await;
    assert!(
        handle_protocol_message(
            request(),
            &mut selected,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &principal,
            "ws"
        )
        .await
    );
    let rejected = rx.recv().await.expect("version rejection");
    assert!(matches!(
        &rejected,
        ServerMsg::ProtocolError { error }
            if error.code == "unsupported_message_version"
    ));
    let rejected_wire = serde_json::to_string(&rejected).unwrap();
    assert!(!rejected_wire.contains("private"));
    assert!(!rejected_wire.contains("PRIVATE_TOKEN"));
    assert!(
        ui.received.lock().await.is_none(),
        "v2 request reached UI service"
    );

    selected = None;
    assert!(
        handle_protocol_message(
            hello(3),
            &mut selected,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &principal,
            "ws"
        )
        .await
    );
    let _ = rx.recv().await;
    assert!(
        handle_protocol_message(
            request(),
            &mut selected,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &principal,
            "ws"
        )
        .await
    );
    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::RegistryAdmin { .. })
    ));
    assert!(
        ui.received.lock().await.is_some(),
        "v3 request was not dispatched"
    );
}

#[tokio::test]
async fn agent_admin_dispatches_through_the_ui_service() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(SessionConfigUi {
            states: Mutex::new(HashMap::new()),
        })),
        None,
    );
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "admin",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::AgentAdmin {
            request: sylvander_protocol::AgentAdminRequest::ActivateRevision {
                agent_id: AgentId::new("oraculo"),
                revision: 5,
                expected_active_revision: 4,
            },
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &principal,
        "websocket-test",
    )
    .await;

    assert!(matches!(
        rx.recv().await.expect("Agent admin response"),
        ServerMsg::AgentAdmin {
            response: sylvander_protocol::AgentAdminResponse::Success {
                result
            }
        } if matches!(
            result.as_ref(),
            sylvander_protocol::AgentAdminResult::RevisionActivated {
                agent_id,
                active_revision: 5,
            } if agent_id == &AgentId::new("oraculo")
        )
    ));
}

#[tokio::test]
async fn identity_binding_round_trip_uses_authenticated_websocket_ingress() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(CredentialRegistryUi {
            received: Mutex::new(None),
        })),
        None,
    );
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "websocket-user",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::IdentityBinding {
            request: Arc::new(sylvander_protocol::IdentityBindingRequest {
                version: sylvander_protocol::IDENTITY_BINDING_PROTOCOL_VERSION,
                action: sylvander_protocol::IdentityBindingAction::Resolve {},
            }),
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &principal,
        "websocket-test",
    )
    .await;

    let response = rx.recv().await.expect("identity response");
    let wire = serde_json::to_string(&response).expect("serialize response");
    let decoded: ServerMsg = serde_json::from_str(&wire).expect("decode response");
    assert!(matches!(
        decoded,
        ServerMsg::IdentityBinding { response }
            if matches!(
                response.as_ref(),
                sylvander_protocol::IdentityBindingResponse::Resolved {
                    binding,
                    ..
                } if binding.user_id == sylvander_protocol::UserId::new("stable-ws-user")
                    && binding.revision == 3
            )
    ));
}

#[tokio::test]
async fn registry_admin_without_ui_service_returns_content_free_error() {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        None,
        None,
    );
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "admin",
        sylvander_protocol::AuthenticationMethod::BearerToken,
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
        &principal,
        "websocket-test",
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

async fn dispatch_registry_admin_as(
    principal: sylvander_protocol::AuthenticatedPrincipal,
) -> ServerMsg {
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(Arc::new(SessionConfigUi {
            states: Mutex::new(HashMap::new()),
        })),
        None,
    );
    let request: ClientMsg = serde_json::from_value(serde_json::json!({
        "type": "registry_admin",
        "request": {
            "operation": "inspect_provider_revision",
            "provider_id": "provider-a",
            "revision": 9
        }
    }))
    .expect("decode registry request");
    let (tx, mut rx) = mpsc::unbounded_channel();
    handle_client_msg(
        request,
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &principal,
        "websocket-test",
    )
    .await;
    rx.recv().await.expect("registry transport response")
}

#[tokio::test]
async fn registry_admin_round_trip_preserves_success_response() {
    let mut principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "admin",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    principal.roles.push("admin".into());
    let response = dispatch_registry_admin_as(principal).await;
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
async fn credential_binding_create_dispatches_reference_without_echoing_it() {
    const BINDING_ID: &str = "credential/private-provider";
    const ENVIRONMENT_NAME: &str = "SYLVANDER_PRIVATE_PROVIDER_TOKEN";

    let ui = Arc::new(CredentialRegistryUi {
        received: Mutex::new(None),
    });
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        Some(ui.clone()),
        None,
    );
    let mut principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "admin",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    principal.roles.push("admin".into());
    let request: ClientMsg = serde_json::from_value(serde_json::json!({
        "type": "registry_admin",
        "request": {
            "operation": "create_credential_binding",
            "binding_id": BINDING_ID,
            "reference": {
                "source": "environment",
                "name": ENVIRONMENT_NAME
            }
        }
    }))
    .expect("decode credential binding request");
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        request,
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &principal,
        "websocket-test",
    )
    .await;

    let received = ui
        .received
        .lock()
        .await
        .take()
        .expect("registry service received request");
    assert!(matches!(
        received,
        sylvander_protocol::RegistryAdminRequest::CreateCredentialBinding {
            binding_id,
            reference:
                sylvander_protocol::CredentialSecretReferenceDraft::Environment { name },
        } if binding_id == BINDING_ID && name == ENVIRONMENT_NAME
    ));

    let response = rx.recv().await.expect("credential registry response");
    assert!(matches!(
        &response,
        ServerMsg::RegistryAdmin {
            response: sylvander_protocol::RegistryAdminResponse::Success { result }
        } if matches!(
            result.as_ref(),
            sylvander_protocol::RegistryAdminResult::CredentialBindingCreated { generation }
                if generation.binding_id_sha256 == "binding-digest"
                    && generation.reference_digest_sha256 == "reference-digest"
                    && generation.reference_configured
        )
    ));
    let wire = serde_json::to_string(&response).expect("encode credential registry response");
    assert!(!wire.contains(BINDING_ID));
    assert!(!wire.contains(ENVIRONMENT_NAME));
}

#[tokio::test]
async fn registry_admin_non_administrator_is_rejected_before_dispatch() {
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "reader",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    assert!(matches!(
        dispatch_registry_admin_as(principal).await,
        ServerMsg::BoundaryDenied { error }
            if error.code == sylvander_protocol::BoundaryErrorCode::Forbidden
                && error.operation == "registry_admin"
    ));
}

#[test]
fn message_limit_is_configurable() {
    let channel = WsChannel::new("127.0.0.1:0".parse().unwrap(), "agent").with_request_limit(4096);
    assert_eq!(channel.max_request_bytes, 4096);
}

#[async_trait]
impl UiService for DenyAgentAccess {
    async fn reject_authentication(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        _: sylvander_protocol::AuthenticationFailure,
    ) -> sylvander_protocol::BoundaryError {
        sylvander_protocol::BoundaryError {
            code: sylvander_protocol::BoundaryErrorCode::RateLimited,
            operation: "authenticate_bearer_token".into(),
            request_id: boundary.request_id.clone(),
            message: "request rate limit exceeded".into(),
            retry_after_ms: Some(1_000),
        }
    }

    async fn authorize_message(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        message: &sylvander_protocol::UiClientMessage,
    ) -> Result<(), sylvander_protocol::BoundaryError> {
        if matches!(
            message,
            sylvander_protocol::UiClientMessage::CreateSession { .. }
        ) {
            Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                "create_session",
            ))
        } else {
            Ok(())
        }
    }

    async fn submit_chat(
        &self,
        boundary: &sylvander_protocol::BoundaryContext,
        _: sylvander_channel::ExternalChatRequest,
    ) -> Result<sylvander_channel::SubmittedChat, sylvander_protocol::BoundaryError> {
        Err(sylvander_protocol::BoundaryError::forbidden(
            boundary,
            "submit_chat",
        ))
    }

    async fn discover_agents(
        &self,
        _: &sylvander_protocol::BoundaryContext,
    ) -> Result<Vec<sylvander_protocol::AgentDescriptor>, sylvander_protocol::BoundaryError> {
        unreachable!()
    }

    async fn create_session(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: sylvander_protocol::SessionCreateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        panic!("denied Agent access must stop before session creation")
    }

    async fn session_config(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: &SessionId,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        unreachable!()
    }

    async fn update_session_config(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: sylvander_protocol::SessionConfigUpdateRequest,
    ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError> {
        unreachable!()
    }

    async fn submit_feedback(
        &self,
        _: &sylvander_protocol::BoundaryContext,
        _: sylvander_protocol::RunFeedback,
    ) -> Result<String, sylvander_protocol::BoundaryError> {
        unreachable!()
    }
}

#[test]
fn approval_reason_is_optional_and_transport_neutral() {
    let legacy: ClientMsg = serde_json::from_value(serde_json::json!({
        "type": "approve",
        "call_id": "call-1",
        "approved": true
    }))
    .expect("legacy approval");
    assert!(matches!(legacy, ClientMsg::Approve { reason: None, .. }));

    let typed: ClientMsg = serde_json::from_value(serde_json::json!({
        "type": "approve",
        "call_id": "call-2",
        "approved": false,
        "reason": "unsafe outside workspace"
    }))
    .expect("typed approval");
    assert!(matches!(
        typed,
        ClientMsg::Approve { reason: Some(reason), .. }
            if reason == "unsafe outside workspace"
    ));
}

#[test]
fn bearer_comparison_checks_content_and_length() {
    assert!(constant_time_eq(b"correct-token", b"correct-token"));
    assert!(!constant_time_eq(b"correct-token", b"wrong-token"));
    assert!(!constant_time_eq(b"token", b"token-extra"));
}

#[tokio::test]
async fn first_chat_cannot_create_a_session_without_agent_access() {
    let sessions: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        sessions.clone(),
        Some(Arc::new(DenyAgentAccess)),
        None,
    );
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "caller",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::Chat {
            text: "hello".into(),
            attachments: Vec::new(),
            session_id: None,
            workspace: None,
        },
        &context,
        &AgentId::new("private-agent"),
        &tx,
        &principal,
        "ws-private",
    )
    .await;

    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::BoundaryDenied { error })
            if error.code == sylvander_protocol::BoundaryErrorCode::Forbidden
    ));
    assert!(sessions.list_persistent().await.unwrap().is_empty());
}

#[tokio::test]
async fn authentication_rejection_uses_runtime_status() {
    let state = AppState {
        ctx: Arc::new(ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
            Some(Arc::new(DenyAgentAccess)),
            None,
        )),
        agent_id: AgentId::new("private-agent"),
        clients: Arc::new(Mutex::new(HashMap::new())),
        next_id: Arc::new(Mutex::new(0)),
        instance_id: "ws-private".into(),
        auth: None,
        max_request_bytes: 4096,
    };
    assert_eq!(
        reject_ws_authentication(&state).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn selection_updates_only_the_addressed_session() {
    let ui = Arc::new(SessionConfigUi {
        states: Mutex::new(HashMap::from([
            ("session-a".into(), config_state("session-a")),
            ("session-b".into(), config_state("session-b")),
        ])),
    });
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
        Some(ui.clone()),
        None,
    );
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "caller",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::SelectModel {
            session_id: Some("session-a".into()),
            model: sylvander_protocol::ModelSelectionInput::Qualified(
                sylvander_protocol::ModelSelection {
                    provider_id: "test".into(),
                    model_id: "thinking-model".into(),
                },
            ),
            reasoning_effort: sylvander_protocol::ReasoningEffort::High,
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &principal,
        "ws-test",
    )
    .await;

    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::SessionConfig { state })
            if state.session_id.0 == "session-a"
                && state.effective.model_id == "thinking-model"
                && state.effective.reasoning_effort
                    == sylvander_protocol::ReasoningEffort::High
    ));
    let states = ui.states.lock().await;
    assert_eq!(states["session-a"].revision, 2);
    assert_eq!(
        states["session-a"].overrides.model,
        Some(sylvander_protocol::ModelSelection {
            provider_id: "test".into(),
            model_id: "thinking-model".into(),
        })
    );
    assert!(states["session-a"].overrides.model_id.is_none());
    assert_eq!(states["session-b"], config_state("session-b"));
}

#[tokio::test]
async fn session_prompt_is_redacted_from_the_websocket_payload() {
    const SENTINEL: &str = "WS_PRIVATE_SESSION_PROMPT_SENTINEL";
    const DIGEST: &str = "ws-public-prompt-digest";
    let mut state = config_state("session-secret");
    state.overrides.system_prompt = Some(SENTINEL.into());
    state.effective.system_prompt_sha256 = DIGEST.into();
    state.effective.prompt_manifest = Some(sylvander_protocol::PromptManifest {
        layers: vec![sylvander_protocol::PromptLayerDigest {
            kind: sylvander_protocol::PromptLayerKind::SessionInput,
            reference: Some("session".into()),
            sha256: DIGEST.into(),
            byte_count: SENTINEL.len() as u64,
        }],
        aggregate_sha256: "aggregate-digest".into(),
        total_bytes: SENTINEL.len() as u64,
    });
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
        Some(Arc::new(SessionConfigUi {
            states: Mutex::new(HashMap::from([("session-secret".into(), state)])),
        })),
        None,
    );
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "caller",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::GetSessionConfig {
            session_id: "session-secret".into(),
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &principal,
        "ws-test",
    )
    .await;
    let response = rx.recv().await.expect("session config response");
    let wire = serde_json::to_string(&response).expect("websocket JSON payload");
    let encoded: serde_json::Value = serde_json::from_str(&wire).expect("session config JSON");

    assert!(!wire.contains(SENTINEL));
    assert!(encoded["state"]["overrides"].get("system_prompt").is_none());
    assert_eq!(
        encoded["state"]["effective"]["system_prompt_sha256"],
        DIGEST
    );
    assert_eq!(
        encoded["state"]["effective"]["prompt_manifest"]["layers"][0]["sha256"],
        DIGEST
    );
}

#[tokio::test]
async fn ambiguous_legacy_selection_fails_without_mutating_session() {
    let ui = Arc::new(SessionConfigUi {
        states: Mutex::new(HashMap::from([(
            "session-a".into(),
            config_state("session-a"),
        )])),
    });
    let context = ChannelContext::with_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
        Some(ui.clone()),
        None,
    );
    let principal = sylvander_protocol::AuthenticatedPrincipal::user(
        "caller",
        sylvander_protocol::AuthenticationMethod::BearerToken,
    );
    let (tx, mut rx) = mpsc::unbounded_channel();

    handle_client_msg(
        ClientMsg::SelectModel {
            session_id: Some("session-a".into()),
            model: sylvander_protocol::ModelSelectionInput::Legacy("shared".into()),
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
        },
        &context,
        &AgentId::new("agent-1"),
        &tx,
        &principal,
        "ws-test",
    )
    .await;

    assert!(matches!(
        rx.recv().await,
        Some(ServerMsg::OperationError { operation, message })
            if operation == "select_model" && message.contains("ambiguous")
    ));
    assert_eq!(
        ui.states.lock().await["session-a"],
        config_state("session-a")
    );
}
