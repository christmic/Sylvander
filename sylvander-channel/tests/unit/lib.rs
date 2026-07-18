use super::*;
use std::sync::Mutex;
use sylvander_agent::bus::InProcessMessageBus;
use sylvander_agent::session_store::SqliteSessionStore;

use sylvander_protocol::{
    AuthenticatedPrincipal, AuthenticationMethod, ClassifiedPreference, IdentityBindingAction,
    LanguageTag, PrivacyClass, UserProfileAction, UserProfileData, UserProfileErrorCode,
    UserProfileOperation,
};

struct DefaultUiService;

struct EnabledIdentityUiService {
    observed_parts: Mutex<Option<(String, String, String)>>,
}

#[async_trait]
impl UiService for DefaultUiService {
    async fn authorize_message(
        &self,
        boundary: &BoundaryContext,
        _: &UiClientMessage,
    ) -> Result<(), BoundaryError> {
        Err(BoundaryError::forbidden(boundary, "test"))
    }

    async fn discover_agents(
        &self,
        boundary: &BoundaryContext,
    ) -> Result<Vec<AgentDescriptor>, BoundaryError> {
        Err(BoundaryError::forbidden(boundary, "test"))
    }

    async fn create_session(
        &self,
        boundary: &BoundaryContext,
        _: SessionCreateRequest,
    ) -> Result<SessionConfigState, BoundaryError> {
        Err(BoundaryError::forbidden(boundary, "test"))
    }

    async fn session_config(
        &self,
        boundary: &BoundaryContext,
        _: &SessionId,
    ) -> Result<SessionConfigState, BoundaryError> {
        Err(BoundaryError::forbidden(boundary, "test"))
    }

    async fn update_session_config(
        &self,
        boundary: &BoundaryContext,
        _: SessionConfigUpdateRequest,
    ) -> Result<SessionConfigState, BoundaryError> {
        Err(BoundaryError::forbidden(boundary, "test"))
    }

    async fn submit_feedback(
        &self,
        boundary: &BoundaryContext,
        _: RunFeedback,
    ) -> Result<String, BoundaryError> {
        Err(BoundaryError::forbidden(boundary, "test"))
    }
}

#[async_trait]
impl UiService for EnabledIdentityUiService {
    async fn authorize_message(
        &self,
        boundary: &BoundaryContext,
        message: &UiClientMessage,
    ) -> Result<(), BoundaryError> {
        DefaultUiService.authorize_message(boundary, message).await
    }

    async fn discover_agents(
        &self,
        boundary: &BoundaryContext,
    ) -> Result<Vec<AgentDescriptor>, BoundaryError> {
        DefaultUiService.discover_agents(boundary).await
    }

    async fn create_session(
        &self,
        boundary: &BoundaryContext,
        request: SessionCreateRequest,
    ) -> Result<SessionConfigState, BoundaryError> {
        DefaultUiService.create_session(boundary, request).await
    }

    async fn session_config(
        &self,
        boundary: &BoundaryContext,
        session_id: &SessionId,
    ) -> Result<SessionConfigState, BoundaryError> {
        DefaultUiService.session_config(boundary, session_id).await
    }

    async fn update_session_config(
        &self,
        boundary: &BoundaryContext,
        request: SessionConfigUpdateRequest,
    ) -> Result<SessionConfigState, BoundaryError> {
        DefaultUiService
            .update_session_config(boundary, request)
            .await
    }

    async fn submit_feedback(
        &self,
        boundary: &BoundaryContext,
        feedback: RunFeedback,
    ) -> Result<String, BoundaryError> {
        DefaultUiService.submit_feedback(boundary, feedback).await
    }

    fn identity_binding_capabilities(&self) -> IdentityBindingCapabilities {
        IdentityBindingCapabilities::current()
    }

    async fn identity_binding(
        &self,
        _: &BoundaryContext,
        identity: AuthenticatedTransportIdentity,
        _: IdentityBindingRequest,
    ) -> IdentityBindingResponse {
        let debug = format!("{identity:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("external-secret"));
        *self.observed_parts.lock().unwrap() = Some(identity.into_parts());
        IdentityBindingResponse::NotLinked { version: 1 }
    }
}

fn resolve_identity_request() -> IdentityBindingRequest {
    IdentityBindingRequest {
        version: IDENTITY_BINDING_PROTOCOL_VERSION,
        action: IdentityBindingAction::Resolve {},
    }
}

#[tokio::test]
async fn agent_admin_default_fails_closed_without_reflecting_request() {
    let boundary = BoundaryContext::unauthenticated("unix", "unix", "request-1");
    let response = DefaultUiService
        .agent_admin(
            &boundary,
            AgentAdminRequest::InspectRevision {
                agent_id: AgentId::new("private-agent"),
                revision: 42,
            },
        )
        .await;
    let json = serde_json::to_string(&response).expect("serialize response");

    assert!(matches!(
        response,
        AgentAdminResponse::Error {
            error: AgentAdminError {
                code: AgentAdminErrorCode::Unauthorized,
                agent_id: None,
                revision: None,
                ..
            }
        }
    ));
    assert!(!json.contains("private-agent"));
    assert!(!json.contains("42"));
}

#[tokio::test]
async fn registry_admin_default_fails_closed_without_reflecting_request() {
    let boundary = BoundaryContext::unauthenticated("unix", "unix", "request-1");
    let response = DefaultUiService
        .registry_admin(
            &boundary,
            RegistryAdminRequest::InspectProviderRevision {
                provider_id: "private-provider".into(),
                revision: 42,
            },
        )
        .await;
    let json = serde_json::to_string(&response).expect("serialize response");

    assert!(matches!(
        response,
        RegistryAdminResponse::Error {
            error: RegistryAdminError {
                code: RegistryAdminErrorCode::Unauthorized,
                provider_id: None,
                revision: None,
                ..
            }
        }
    ));
    assert!(!json.contains("private-provider"));
    assert!(!json.contains("42"));
}

#[tokio::test]
async fn external_chat_fails_closed_without_runtime_authorizer() {
    let context = ChannelContext::new(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
    );
    let boundary = BoundaryContext::authenticated(
        AuthenticatedPrincipal::user("telegram:bot-a:42", AuthenticationMethod::PlatformIdentity),
        "bot-a",
        "telegram",
        "update-1",
    );

    let error = submit_external_chat(
        &context,
        &boundary,
        ExternalChatRequest {
            existing_session: None,
            agent_id: AgentId::new("assistant"),
            label: "telegram-42".into(),
            overrides: SessionConfigOverrides::default(),
            text: "hello".into(),
            attachments: Vec::new(),
            external_meta: BTreeMap::new(),
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code, BoundaryErrorCode::InvalidScope);
}

#[test]
fn channel_defaults_fill_only_missing_session_fields() {
    let mut overrides = SessionConfigOverrides {
        execution_target: Some("explicit-target".into()),
        ..SessionConfigOverrides::default()
    };
    let defaults = SessionConfigOverrides {
        user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "channel-target".into(),
            path: "/workspace/channel".into(),
            read_only: true,
            instruction_focus: Some("src".into()),
        }),
        execution_target: Some("channel-target".into()),
        ..SessionConfigOverrides::default()
    };

    inherit_session_defaults(&mut overrides, &defaults);

    assert_eq!(
        overrides
            .user_workspace
            .as_ref()
            .map(|workspace| workspace.path.as_path()),
        Some(std::path::Path::new("/workspace/channel"))
    );
    assert_eq!(
        overrides.execution_target.as_deref(),
        Some("explicit-target")
    );
}

#[test]
fn external_controls_are_typed_and_require_an_existing_session() {
    let session = SessionId::new("session-1");
    assert!(matches!(
        parse_external_control("/approve batch-1 session", Some(&session)),
        Some(Ok(UiClientMessage::Approve {
            approved: true,
            scope: sylvander_protocol::ApprovalScope::Session,
            ..
        }))
    ));
    assert!(matches!(
        parse_external_control("/deny batch-1 unsafe path", Some(&session)),
        Some(Ok(UiClientMessage::Approve {
            approved: false,
            reason: Some(reason),
            ..
        })) if reason == "unsafe path"
    ));
    assert!(matches!(
        parse_external_control("/answer ask-1 use option two", Some(&session)),
        Some(Ok(UiClientMessage::Answer { answer, .. })) if answer == "use option two"
    ));
    assert!(matches!(
        parse_external_control("/interrupt", None),
        Some(Err("no active session for this control"))
    ));
    assert!(parse_external_control("/new", Some(&session)).is_none());
}

#[tokio::test]
async fn identity_binding_defaults_to_no_capability_and_denial() {
    let context = ChannelContext::with_runtime_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
        Arc::new(DefaultUiService),
        None,
    );
    let boundary = BoundaryContext::authenticated(
        AuthenticatedPrincipal::user("external-secret", AuthenticationMethod::PlatformIdentity),
        "bot-a",
        "telegram",
        "update-1",
    );

    assert!(context.identity_binding_capabilities().versions.is_empty());
    let response = context
        .submit_identity_binding(&boundary, resolve_identity_request())
        .await;
    assert!(matches!(
        response,
        IdentityBindingResponse::Error {
            error: IdentityBindingError {
                code: IdentityBindingErrorCode::ServiceUnavailable,
                ..
            },
            ..
        }
    ));
    assert!(!format!("{response:?}").contains("external-secret"));
}

#[tokio::test]
async fn user_profile_defaults_to_no_capability_and_content_safe_denial() {
    let boundary = BoundaryContext::authenticated(
        AuthenticatedPrincipal::user("stable-user", AuthenticationMethod::UnixPeer),
        "local",
        "unix",
        "request-1",
    );
    let private_marker = "private-language-marker";
    let request = UserProfileRequest {
        version: USER_PROFILE_PROTOCOL_VERSION,
        action: UserProfileAction::Create {
            profile: UserProfileData {
                preferred_language: Some(ClassifiedPreference {
                    value: LanguageTag::new(private_marker).unwrap(),
                    privacy_class: PrivacyClass::Restricted,
                }),
                ..UserProfileData::default()
            },
        },
    };

    assert!(
        DefaultUiService
            .user_profile_capabilities()
            .versions
            .is_empty()
    );
    let response = DefaultUiService.user_profile(&boundary, request).await;
    assert!(matches!(
        response,
        UserProfileResponse::Error {
            version: USER_PROFILE_PROTOCOL_VERSION,
            error: UserProfileError {
                code: UserProfileErrorCode::ServiceUnavailable,
                operation: UserProfileOperation::Create,
                current_revision: None,
                retry_after_ms: None,
            }
        }
    ));
    let encoded = serde_json::to_string(&response).unwrap();
    assert!(!encoded.contains(private_marker));
    assert!(!format!("{response:?}").contains(private_marker));
}

#[tokio::test]
async fn authenticated_channel_context_derives_the_only_transport_identity() {
    let service = Arc::new(EnabledIdentityUiService {
        observed_parts: Mutex::new(None),
    });
    let context = ChannelContext::with_runtime_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
        service.clone(),
        None,
    );
    let boundary = BoundaryContext::authenticated(
        AuthenticatedPrincipal::user("external-secret", AuthenticationMethod::PlatformIdentity),
        "bot-a",
        "telegram",
        "update-1",
    );

    let response = context
        .submit_identity_binding(&boundary, resolve_identity_request())
        .await;
    assert_eq!(response, IdentityBindingResponse::NotLinked { version: 1 });
    assert_eq!(
        service.observed_parts.lock().unwrap().as_ref(),
        Some(&("telegram".into(), "bot-a".into(), "external-secret".into()))
    );
}

#[tokio::test]
async fn unauthenticated_or_non_user_ingress_never_reaches_identity_service() {
    let service = Arc::new(EnabledIdentityUiService {
        observed_parts: Mutex::new(None),
    });
    let context = ChannelContext::with_runtime_services(
        Arc::new(InProcessMessageBus::new()),
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
        service.clone(),
        None,
    );
    let unauthenticated = BoundaryContext::unauthenticated("bot-a", "telegram", "update-1");
    let response = context
        .submit_identity_binding(&unauthenticated, resolve_identity_request())
        .await;
    assert!(matches!(
        response,
        IdentityBindingResponse::Error {
            error: IdentityBindingError {
                code: IdentityBindingErrorCode::Unauthenticated,
                ..
            },
            ..
        }
    ));

    let mut channel =
        AuthenticatedPrincipal::user("external-secret", AuthenticationMethod::PlatformIdentity);
    channel.kind = PrincipalKind::Channel;
    let response = context
        .submit_identity_binding(
            &BoundaryContext::authenticated(channel, "bot-a", "telegram", "update-2"),
            resolve_identity_request(),
        )
        .await;
    assert!(matches!(
        response,
        IdentityBindingResponse::Error {
            error: IdentityBindingError {
                code: IdentityBindingErrorCode::Forbidden,
                ..
            },
            ..
        }
    ));
    assert!(service.observed_parts.lock().unwrap().is_none());
}
