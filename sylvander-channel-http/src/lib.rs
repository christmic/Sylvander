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
use axum::response::sse::{Event, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_stream::StreamExt;

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
}

impl HttpChannel {
    pub fn new(addr: SocketAddr, agent_id: impl Into<sylvander_agent::spec::AgentId>) -> Self {
        Self { addr, agent_id: agent_id.into() }
    }
}

#[async_trait]
impl Channel for HttpChannel {
    fn name(&self) -> &str { "http" }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let agent = self.agent_id.clone();
        let state = Arc::new(AppState {
            ctx: Arc::new(ctx),
            agent_id: agent,
            sessions: Mutex::new(std::collections::HashMap::new()),
        });

        let app = Router::new()
            .route("/health", get(|| async { Json(serde_json::json!({"status":"ok"})) }))
            .route("/chat", post(chat))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind(self.addr).await.unwrap();
        tracing::info!(addr = %self.addr, "http channel listening");
        axum::serve(listener, app).await.unwrap();
    }
}

struct AppState {
    ctx: Arc<ChannelContext>,
    agent_id: sylvander_agent::spec::AgentId,
    sessions: Mutex<std::collections::HashMap<String, SessionId>>,
}

async fn chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
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
    use sylvander_agent::bus::SystemMessage;
    use sylvander_agent::session::SessionMetadata;
    use std::path::PathBuf;
    let _ = state.ctx.bus.publish(sylvander_agent::bus::BusMessage {
        session_id: sid.clone(),
        sender: sylvander_agent::bus::Sender::System,
        recipient: sylvander_agent::bus::Recipient::Agent(
            state.agent_id.clone(),
        ),
        kind: sylvander_agent::bus::MessageKind::System(SystemMessage::JoinSession {
            session_id: sid.clone(),
            metadata: SessionMetadata {
                workspace: PathBuf::from("/tmp"),
                name: "http".into(),
                user_id: "http-user".into(),
            },
        }),
        payload: String::new(),
        timestamp: sylvander_agent::session::now_secs(),
        id: sylvander_agent::bus::MessageId::new(),
    }).await;

    let _ = state
        .ctx
        .bus
        .publish(BusMessage::user_chat(sid.clone(), "http-user", &req.message))
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

    Sse::new(stream)
}
