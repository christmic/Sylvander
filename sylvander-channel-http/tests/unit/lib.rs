use super::*;
use sylvander_channel::credential::{
    CredentialLeaseBundle, CredentialLeaseError, CredentialLeaseRequest, CredentialLeaseSource,
};
use sylvander_channel::{ChannelHost, InProcessMessageBus};

struct DenyAgentAccess;

struct RotatingLeaseSource {
    state: std::sync::Mutex<(u64, String, bool)>,
}

fn bearer_lease(instance_id: &str, value: &str) -> BearerLease {
    BearerLease {
        source: Arc::new(RotatingLeaseSource {
            state: std::sync::Mutex::new((1, value.into(), false)),
        }),
        request: CredentialLeaseRequest::new(instance_id, ["bearer_token"]).unwrap(),
    }
}

#[async_trait]
impl CredentialLeaseSource for RotatingLeaseSource {
    async fn lease(
        &self,
        request: &CredentialLeaseRequest,
    ) -> Result<CredentialLeaseBundle, CredentialLeaseError> {
        let (generation, value, unavailable) = self.state.lock().unwrap().clone();
        if unavailable {
            return Err(CredentialLeaseError::Unavailable);
        }
        let now: i64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .try_into()
            .unwrap();
        CredentialLeaseBundle::new(
            generation,
            generation,
            now,
            now + 30,
            [(request.slots[0].clone(), value.into_bytes())],
        )
    }
}

#[test]
fn request_limit_is_configurable() {
    let channel =
        HttpChannel::new("127.0.0.1:0".parse().unwrap(), "agent").with_request_limit(4096);
    assert_eq!(channel.max_request_bytes, 4096);
}

#[async_trait]
impl ChannelHost for DenyAgentAccess {
    async fn reject_authentication(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        _: sylvander_api::AuthenticationFailure,
    ) -> sylvander_api::BoundaryError {
        sylvander_api::BoundaryError {
            code: sylvander_api::BoundaryErrorCode::RateLimited,
            operation: "authenticate_bearer_token".into(),
            request_id: boundary.request_id.clone(),
            message: "request rate limit exceeded".into(),
            retry_after_ms: Some(1_000),
        }
    }

    async fn authorize_message(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        message: &sylvander_api::UiClientMessage,
    ) -> Result<(), sylvander_api::BoundaryError> {
        if matches!(
            message,
            sylvander_api::UiClientMessage::CreateSession { .. }
        ) {
            Err(sylvander_api::BoundaryError::forbidden(
                boundary,
                "create_session",
            ))
        } else {
            Ok(())
        }
    }

    async fn submit_chat(
        &self,
        boundary: &sylvander_api::BoundaryContext,
        _: sylvander_channel::ExternalChatRequest,
    ) -> Result<sylvander_channel::SubmittedChat, sylvander_api::BoundaryError> {
        Err(sylvander_api::BoundaryError::forbidden(
            boundary,
            "submit_chat",
        ))
    }

    async fn discover_agents(
        &self,
        _: &sylvander_api::BoundaryContext,
    ) -> Result<Vec<sylvander_api::AgentDescriptor>, sylvander_api::BoundaryError> {
        unreachable!()
    }

    async fn create_session(
        &self,
        _: &sylvander_api::BoundaryContext,
        _: sylvander_api::SessionCreateRequest,
    ) -> Result<sylvander_api::SessionConfigState, sylvander_api::BoundaryError> {
        panic!("denied Agent access must stop before session creation")
    }

    async fn session_config(
        &self,
        _: &sylvander_api::BoundaryContext,
        _: &SessionId,
    ) -> Result<sylvander_api::SessionConfigState, sylvander_api::BoundaryError> {
        unreachable!()
    }

    async fn update_session_config(
        &self,
        _: &sylvander_api::BoundaryContext,
        _: sylvander_api::SessionConfigUpdateRequest,
    ) -> Result<sylvander_api::SessionConfigState, sylvander_api::BoundaryError> {
        unreachable!()
    }

    async fn submit_feedback(
        &self,
        _: &sylvander_api::BoundaryContext,
        _: sylvander_api::RunFeedback,
    ) -> Result<String, sylvander_api::BoundaryError> {
        unreachable!()
    }
}

#[test]
fn bearer_comparison_rejects_wrong_content_and_length() {
    assert!(constant_time_eq(b"secret", b"secret"));
    assert!(!constant_time_eq(b"secret", b"wrong!"));
    assert!(!constant_time_eq(b"secret", b"secret-extra"));
}

#[tokio::test]
async fn live_bearer_lease_rotates_and_fails_closed_without_restart() {
    let source = Arc::new(RotatingLeaseSource {
        state: std::sync::Mutex::new((1, "first-token".into(), false)),
    });
    let state = AppState {
        ctx: Arc::new(ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Some("test".into()),
            None,
            None,
        )),
        agent_id: sylvander_api::AgentId::new("agent"),
        sessions: Mutex::new(std::collections::HashMap::new()),
        instance_id: "http-primary".into(),
        principal_id: Some("caller".into()),
        bearer_lease: Some(BearerLease {
            source: source.clone(),
            request: CredentialLeaseRequest::new("http-primary", ["bearer_token"]).unwrap(),
        }),
        operational_health: None,
    };
    let headers = |value: &str| {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {value}").parse().unwrap(),
        );
        headers
    };

    assert!(
        authenticate(&state, &headers("first-token"))
            .await
            .is_some()
    );
    *source.state.lock().unwrap() = (2, "second-token".into(), false);
    assert!(
        authenticate(&state, &headers("first-token"))
            .await
            .is_none()
    );
    assert!(
        authenticate(&state, &headers("second-token"))
            .await
            .is_some()
    );
    *source.state.lock().unwrap() = (2, "second-token".into(), true);
    assert!(
        authenticate(&state, &headers("second-token"))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn first_chat_cannot_create_a_session_without_agent_access() {
    let state = Arc::new(AppState {
        ctx: Arc::new(ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Some("test".into()),
            Some(Arc::new(DenyAgentAccess)),
            None,
        )),
        agent_id: sylvander_api::AgentId::new("private-agent"),
        sessions: Mutex::new(std::collections::HashMap::new()),
        instance_id: "http-private".into(),
        principal_id: Some("caller".into()),
        bearer_lease: Some(bearer_lease("http-private", "secret")),
        operational_health: None,
    });
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        "Bearer secret".parse().unwrap(),
    );

    let result = chat(
        State(state.clone()),
        Extension(
            authenticate(&state, &headers)
                .await
                .expect("test bearer credential is valid"),
        ),
        Json(ChatRequest {
            session_id: "client-session".into(),
            message: "hello".into(),
        }),
    )
    .await;

    assert!(matches!(result, Err(StatusCode::FORBIDDEN)));
    assert!(state.sessions.lock().await.is_empty());
}

#[tokio::test]
async fn authentication_rejection_uses_runtime_status() {
    let state = AppState {
        ctx: Arc::new(ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Some("test".into()),
            Some(Arc::new(DenyAgentAccess)),
            None,
        )),
        agent_id: sylvander_api::AgentId::new("private-agent"),
        sessions: Mutex::new(std::collections::HashMap::new()),
        instance_id: "http-private".into(),
        principal_id: Some("caller".into()),
        bearer_lease: Some(bearer_lease("http-private", "secret")),
        operational_health: None,
    };
    assert_eq!(
        reject_http_authentication(&state).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn operational_health_controls_readiness_and_metrics() {
    let state = AppState {
        ctx: Arc::new(ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Some("test".into()),
            None,
            None,
        )),
        agent_id: sylvander_api::AgentId::new("agent"),
        sessions: Mutex::new(std::collections::HashMap::new()),
        instance_id: "http".into(),
        principal_id: None,
        bearer_lease: None,
        operational_health: Some(Arc::new(|| {
            Box::pin(async {
                Ok(OperationalHealth {
                    ready: false,
                    agents: 2,
                    persistent_sessions: 3,
                    ready_channels: 1,
                    total_channels: 2,
                    bus_subscribers: 4,
                    bus_capacity: 256,
                    published_messages: 8,
                    backpressure_rejections: 1,
                })
            })
        })),
    };
    assert_eq!(
        operational_health(&state, true).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        metrics(State(Arc::new(state))).await.status(),
        StatusCode::OK
    );
}
