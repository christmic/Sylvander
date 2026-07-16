//! HTTP debug channel — curl-friendly API with SSE streaming.
//!
//! ```bash
//! curl -N -X POST http://localhost:8080/chat \
//!   -H 'Content-Type: application/json' \
//!   -d '{"session_id":"test","message":"hello"}'
//! ```

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::Mutex;

use sylvander_agent::bus::{MessageKind, StreamEvent};
use sylvander_agent::spec::SessionId;
use sylvander_channel::{Channel, ChannelContext, ExternalChatRequest, submit_external_chat};

#[derive(Deserialize)]
struct ChatRequest {
    session_id: String,
    message: String,
}

pub struct HttpChannel {
    addr: SocketAddr,
    agent_id: sylvander_agent::spec::AgentId,
    instance_id: String,
    principal_id: Option<String>,
    bearer_token: Option<String>,
    max_request_bytes: usize,
}

impl HttpChannel {
    pub fn new(addr: SocketAddr, agent_id: impl Into<sylvander_agent::spec::AgentId>) -> Self {
        Self {
            addr,
            agent_id: agent_id.into(),
            instance_id: "http".into(),
            principal_id: None,
            bearer_token: None,
            max_request_bytes: 1024 * 1024,
        }
    }

    #[must_use]
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }

    pub fn with_bearer_auth(
        mut self,
        instance_id: impl Into<String>,
        principal_id: impl Into<String>,
        bearer_token: impl Into<String>,
    ) -> Self {
        self.instance_id = instance_id.into();
        self.principal_id = Some(principal_id.into());
        self.bearer_token = Some(bearer_token.into());
        self
    }
}

#[async_trait]
impl Channel for HttpChannel {
    fn name(&self) -> &'static str {
        "http"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let agent = self.agent_id.clone();
        let state = Arc::new(AppState {
            ctx: Arc::new(ctx),
            agent_id: agent,
            sessions: Mutex::new(std::collections::HashMap::new()),
            instance_id: self.instance_id.clone(),
            principal_id: self.principal_id.clone(),
            bearer_token: self.bearer_token.clone(),
        });

        let chat_routes =
            Router::new()
                .route("/chat", post(chat))
                .route_layer(middleware::from_fn_with_state(
                    state.clone(),
                    require_http_authentication,
                ));
        let app = Router::new()
            .route(
                "/health",
                get(|| async { Json(serde_json::json!({"status":"ok"})) }),
            )
            .merge(chat_routes)
            .layer(DefaultBodyLimit::max(self.max_request_bytes))
            .with_state(state.clone());

        let listener = match tokio::net::TcpListener::bind(self.addr).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, addr = %self.addr, "http channel bind failed");
                return;
            }
        };
        tracing::info!(addr = %self.addr, "http channel listening");
        state.ctx.mark_ready();
        let shutdown = state.ctx.clone();
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.shutdown_requested().await })
            .await
        {
            tracing::warn!(%error, "http channel server failed");
        }
    }
}

async fn require_http_authentication(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if authenticate(&state, &headers).is_some() {
        return next.run(request).await;
    }
    reject_http_authentication(&state).await.into_response()
}

async fn reject_http_authentication(state: &AppState) -> StatusCode {
    let boundary = sylvander_protocol::BoundaryContext::unauthenticated(
        &state.instance_id,
        "http",
        uuid::Uuid::new_v4().to_string(),
    );
    if let Some(ui) = &state.ctx.ui {
        let error = ui
            .reject_authentication(
                &boundary,
                sylvander_protocol::AuthenticationFailure::new(
                    sylvander_protocol::AuthenticationMethod::BearerToken,
                ),
            )
            .await;
        boundary_status(error)
    } else {
        StatusCode::UNAUTHORIZED
    }
}

struct AppState {
    ctx: Arc<ChannelContext>,
    agent_id: sylvander_agent::spec::AgentId,
    sessions: Mutex<std::collections::HashMap<String, SessionId>>,
    instance_id: String,
    principal_id: Option<String>,
    bearer_token: Option<String>,
}

async fn chat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ChatRequest>,
) -> Result<
    Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>>,
    StatusCode,
> {
    let principal = authenticate(&state, &headers).ok_or(StatusCode::UNAUTHORIZED)?;
    let mut aliases = state.sessions.lock().await;
    let existing_session = aliases.get(&req.session_id).cloned();
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        principal.clone(),
        &state.instance_id,
        "http",
        uuid::Uuid::new_v4().to_string(),
    );
    let submitted = submit_external_chat(
        &state.ctx,
        &boundary,
        ExternalChatRequest {
            existing_session: existing_session.clone(),
            agent_id: state.agent_id.clone(),
            label: "HTTP session".into(),
            overrides: sylvander_protocol::SessionConfigOverrides::default(),
            text: req.message.clone(),
            attachments: Vec::new(),
            external_meta: BTreeMap::from([
                ("channel_instance_id".into(), state.instance_id.clone()),
                ("http_session_key".into(), req.session_id.clone()),
            ]),
        },
    )
    .await
    .map_err(boundary_status)?;
    let sid = submitted.session_id;
    let mut event_rx = submitted.events;
    if existing_session.is_none() {
        aliases.insert(req.session_id.clone(), sid.clone());
    }
    drop(aliases);

    let stream = async_stream::stream! {
        while let Some(msg) = event_rx.recv().await {
            if let MessageKind::Stream(ev) = &msg.kind {
                let event = match ev {
                    StreamEvent::TextDelta { delta } =>
                        Event::default().data(delta.as_str()).event("text"),
                    StreamEvent::ToolCall { tool_name, .. } =>
                        Event::default().data(tool_name.as_str()).event("tool_call"),
                    StreamEvent::ToolResult { tool_name, output, .. } => {
                        let d = serde_json::json!({"tool":tool_name,"output":output});
                        Event::default().data(d.to_string()).event("tool_result")
                    }
                    StreamEvent::Done { text } => {
                        yield Ok(Event::default().data(text.as_str()).event("done"));
                        break;
                    }
                    StreamEvent::IterationStart { iteration } =>
                        Event::default().data(iteration.to_string()).event("iteration_start"),
                    _ => continue,
                };
                yield Ok(event);
            }
        }
    };

    Ok(Sse::new(stream))
}

fn boundary_status(error: sylvander_protocol::BoundaryError) -> StatusCode {
    match error.code {
        sylvander_protocol::BoundaryErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
        sylvander_protocol::BoundaryErrorCode::Forbidden => StatusCode::FORBIDDEN,
        sylvander_protocol::BoundaryErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        sylvander_protocol::BoundaryErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        sylvander_protocol::BoundaryErrorCode::InvalidScope => StatusCode::BAD_REQUEST,
    }
}

fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<sylvander_protocol::AuthenticatedPrincipal> {
    let expected = state.bearer_token.as_deref()?;
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    if !constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
        return None;
    }
    Some(sylvander_protocol::AuthenticatedPrincipal::user(
        state.principal_id.clone()?,
        sylvander_protocol::AuthenticationMethod::BearerToken,
    ))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut different = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        different |= usize::from(
            left.get(index).copied().unwrap_or_default()
                ^ right.get(index).copied().unwrap_or_default(),
        );
    }
    different == 0
}

#[cfg(test)]
mod tests {
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
        ) -> Result<Vec<sylvander_protocol::AgentDescriptor>, sylvander_protocol::BoundaryError>
        {
            unreachable!()
        }

        async fn create_session(
            &self,
            _: &sylvander_protocol::BoundaryContext,
            _: sylvander_protocol::SessionCreateRequest,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
            panic!("denied Agent access must stop before session creation")
        }

        async fn session_config(
            &self,
            _: &sylvander_protocol::BoundaryContext,
            _: &SessionId,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
            unreachable!()
        }

        async fn update_session_config(
            &self,
            _: &sylvander_protocol::BoundaryContext,
            _: sylvander_protocol::SessionConfigUpdateRequest,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
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
        };
        assert_eq!(
            reject_http_authentication(&state).await,
            StatusCode::TOO_MANY_REQUESTS
        );
    }
}
