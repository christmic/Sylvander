use super::*;
use sylvander_agent::bus::InProcessMessageBus;
use sylvander_agent::session_store::{SessionStore, SqliteSessionStore};
use sylvander_channel::UiService;

struct DenyAgentAccess;

#[test]
fn request_limit_is_configurable() {
    let channel =
        HttpChannel::new("127.0.0.1:0".parse().unwrap(), "agent").with_request_limit(4096);
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
fn bearer_comparison_rejects_wrong_content_and_length() {
    assert!(constant_time_eq(b"secret", b"secret"));
    assert!(!constant_time_eq(b"secret", b"wrong!"));
    assert!(!constant_time_eq(b"secret", b"secret-extra"));
}

#[tokio::test]
async fn first_chat_cannot_create_a_session_without_agent_access() {
    let sessions: Arc<dyn SessionStore> =
        Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    let state = Arc::new(AppState {
        ctx: Arc::new(ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            sessions.clone(),
            Some(Arc::new(DenyAgentAccess)),
            None,
        )),
        agent_id: sylvander_agent::spec::AgentId::new("private-agent"),
        sessions: Mutex::new(std::collections::HashMap::new()),
        instance_id: "http-private".into(),
        principal_id: Some("caller".into()),
        bearer_token: Some("secret".into()),
        operational_health: None,
    });
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        "Bearer secret".parse().unwrap(),
    );

    let result = chat(
        State(state.clone()),
        headers,
        Json(ChatRequest {
            session_id: "client-session".into(),
            message: "hello".into(),
        }),
    )
    .await;

    assert!(matches!(result, Err(StatusCode::FORBIDDEN)));
    assert!(state.sessions.lock().await.is_empty());
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
        agent_id: sylvander_agent::spec::AgentId::new("private-agent"),
        sessions: Mutex::new(std::collections::HashMap::new()),
        instance_id: "http-private".into(),
        principal_id: Some("caller".into()),
        bearer_token: Some("secret".into()),
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
            Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
            None,
            None,
        )),
        agent_id: sylvander_agent::spec::AgentId::new("agent"),
        sessions: Mutex::new(std::collections::HashMap::new()),
        instance_id: "http".into(),
        principal_id: None,
        bearer_token: None,
        operational_health: Some(Arc::new(|| {
            Box::pin(async {
                Ok(OperationalHealth {
                    ready: false,
                    agents: 2,
                    persistent_sessions: 3,
                    ephemeral_sessions: 1,
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
