//! HTTP debug channel — curl-friendly API with SSE streaming.
//!
//! ```bash
//! curl -N -X POST http://localhost:8080/chat \
//!   -H 'Content-Type: application/json' \
//!   -d '{"session_id":"test","message":"hello"}'
//! ```

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::Mutex;

use sylvander_agent::bus::{BusMessage, MessageKind, StreamEvent, SubscriptionFilter};
use sylvander_agent::spec::SessionId;
use sylvander_channel::{Channel, ChannelContext};

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
}

impl HttpChannel {
    pub fn new(addr: SocketAddr, agent_id: impl Into<sylvander_agent::spec::AgentId>) -> Self {
        Self {
            addr,
            agent_id: agent_id.into(),
            instance_id: "http".into(),
            principal_id: None,
            bearer_token: None,
        }
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
    fn name(&self) -> &str {
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

        let app = Router::new()
            .route(
                "/health",
                get(|| async { Json(serde_json::json!({"status":"ok"})) }),
            )
            .route("/chat", post(chat))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind(self.addr).await.unwrap();
        tracing::info!(addr = %self.addr, "http channel listening");
        state.ctx.mark_ready();
        let shutdown = state.ctx.clone();
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.shutdown_requested().await })
            .await
            .unwrap();
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
    let existing_session = state.sessions.lock().await.get(&req.session_id).cloned();
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        principal.clone(),
        &state.instance_id,
        "http",
        uuid::Uuid::new_v4().to_string(),
    );
    let message = sylvander_protocol::UiClientMessage::Chat {
        text: req.message.clone(),
        attachments: Vec::new(),
        session_id: existing_session.as_ref().map(|session| session.0.clone()),
        workspace: None,
    };
    let ui = state
        .ctx
        .ui
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    if let Err(error) = ui.authorize_message(&boundary, &message).await {
        return Err(match error.code {
            sylvander_protocol::BoundaryErrorCode::Unauthenticated => StatusCode::UNAUTHORIZED,
            sylvander_protocol::BoundaryErrorCode::Forbidden => StatusCode::FORBIDDEN,
            sylvander_protocol::BoundaryErrorCode::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            sylvander_protocol::BoundaryErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            sylvander_protocol::BoundaryErrorCode::InvalidScope => StatusCode::BAD_REQUEST,
        });
    }
    let sid = {
        let mut sessions = state.sessions.lock().await;
        sessions
            .entry(req.session_id.clone())
            .or_insert_with(|| SessionId::new(uuid::Uuid::new_v4().to_string()))
            .clone()
    };

    let mut event_rx = state
        .ctx
        .bus
        .subscribe(SubscriptionFilter {
            session_ids: Some(vec![sid.clone()]),
            recipients: None,
            kinds: None,
        })
        .await
        .unwrap();

    // Ensure agent is in this session before sending message
    use std::path::PathBuf;
    use sylvander_agent::bus::SystemMessage;
    use sylvander_agent::session::SessionMetadata;
    let _ = state
        .ctx
        .bus
        .publish(sylvander_agent::bus::BusMessage {
            session_id: sid.clone(),
            sender: sylvander_agent::bus::Sender::System,
            recipient: sylvander_agent::bus::Recipient::Agent(state.agent_id.clone()),
            kind: sylvander_agent::bus::MessageKind::System(SystemMessage::JoinSession {
                session_id: sid.clone(),
                metadata: SessionMetadata {
                    workspace: PathBuf::from("/tmp"),
                    name: "http".into(),
                    user_id: principal.id.0.clone(),
                },
            }),
            payload: String::new(),
            attachments: Vec::new(),
            timestamp: sylvander_agent::session::now_secs(),
            id: sylvander_agent::bus::MessageId::new(),
        })
        .await;

    let _ = state
        .ctx
        .bus
        .publish(BusMessage::user_chat(
            sid.clone(),
            &principal.id.0,
            &req.message,
        ))
        .await;

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
    use super::constant_time_eq;

    #[test]
    fn bearer_comparison_rejects_wrong_content_and_length() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"wrong!"));
        assert!(!constant_time_eq(b"secret", b"secret-extra"));
    }
}
