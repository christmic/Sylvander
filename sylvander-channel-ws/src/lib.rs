//! WebSocket channel — desktop client integration.
//!
//! # Protocol
//!
//! JSON messages over a single WebSocket connection, full-duplex.
//!
//! ## Client → Server (commands)
//! ```json
//! {"type":"chat","text":"hello","session_id":"optional"}
//! {"type":"approve","call_id":"...","approved":true}
//! {"type":"list_sessions"}
//! {"type":"ping"}
//! ```
//!
//! ## Server → Client (events)
//! ```json
//! {"type":"session_created","session_id":"..."}
//! {"type":"text_delta","session_id":"...","delta":"..."}
//! {"type":"tool_call","session_id":"...","tool_name":"..."}
//! {"type":"tool_result","session_id":"...","tool_name":"...","output":"...","is_error":false}
//! {"type":"tool_rejected","session_id":"...","tool_name":"...","reason":"..."}
//! {"type":"tool_approval_required","session_id":"...","batch_id":"...","tools":[...]}
//! {"type":"iteration_start","session_id":"...","iteration":1}
//! {"type":"done","session_id":"...","text":"..."}
//! {"type":"error","session_id":"...","message":"..."}
//! {"type":"pong"}
//! ```

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

use sylvander_agent::bus::{
    BusMessage, MessageKind, StreamEvent, SubscriptionFilter, SystemMessage,
};
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{Channel, ChannelContext};
use sylvander_protocol::{
    UiClientMessage as ClientMsg, UiServerMessage as ServerMsg, UiToolInfo as ToolInfo,
};

// ===========================================================================
// Channel
// ===========================================================================

pub struct WsChannel {
    addr: SocketAddr,
    agent_id: AgentId,
}

impl WsChannel {
    pub fn new(addr: SocketAddr, agent_id: impl Into<AgentId>) -> Self {
        Self {
            addr,
            agent_id: agent_id.into(),
        }
    }
}

#[async_trait]
impl Channel for WsChannel {
    fn name(&self) -> &str {
        "ws"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);

        // Active clients: client_id → tx
        let clients: Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<ServerMsg>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let next_id: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));

        // HTTP server
        let state = Arc::new(AppState {
            ctx,
            agent_id: self.agent_id.clone(),
            clients: clients.clone(),
            next_id: next_id.clone(),
        });

        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind(self.addr).await.unwrap();
        info!(addr = %self.addr, "ws channel listening");
        state.ctx.mark_ready();
        let shutdown = state.ctx.clone();
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.shutdown_requested().await })
            .await
            .unwrap();
    }
}

// ===========================================================================
// App state
// ===========================================================================

struct AppState {
    ctx: Arc<ChannelContext>,
    agent_id: AgentId,
    clients: Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<ServerMsg>>>>,
    next_id: Arc<Mutex<u64>>,
}

// ===========================================================================
// WebSocket upgrade
// ===========================================================================

async fn ws_handler(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let client_id = {
        let mut id = state.next_id.lock().await;
        *id += 1;
        *id
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    state.clients.lock().await.insert(client_id, tx.clone());
    info!(client_id, "ws client connected");

    let (mut ws_tx, mut ws_rx) = socket.split();

    // Writer: send ServerMsg to client
    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Ok(s) = serde_json::to_string(&msg) {
                if ws_tx.send(Message::Text(s.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    // Reader: receive ClientMsg from client
    let clients = state.clients.clone();
    let ctx = state.ctx.clone();
    let agent_id = state.agent_id.clone();
    let client_id_for_cleanup = client_id;
    while let Some(msg) = ws_rx.next().await {
        let msg = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(_) => break,
        };

        let parsed: ClientMsg = match serde_json::from_str(&msg) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, msg = %msg, "ws: bad json");
                continue;
            }
        };
        handle_client_msg(parsed, &ctx, &agent_id, &tx).await;
    }

    // Cleanup
    write_task.abort();
    clients.lock().await.remove(&client_id_for_cleanup);
    info!(client_id = client_id_for_cleanup, "ws client disconnected");
}

async fn handle_client_msg(
    msg: ClientMsg,
    ctx: &ChannelContext,
    agent_id: &AgentId,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    match msg {
        ClientMsg::Hello { protocol } => match sylvander_protocol::negotiate_ui_protocol(&protocol)
        {
            Ok(version) => {
                let _ = tx.send(ServerMsg::Welcome {
                    protocol: sylvander_protocol::UiProtocolWelcome {
                        server_name: "sylvander-server".into(),
                        version,
                        capabilities: vec![
                            "agent_discovery".into(),
                            "session_config".into(),
                            "feedback".into(),
                        ],
                    },
                });
            }
            Err(error) => {
                let _ = tx.send(ServerMsg::ProtocolError { error });
            }
        },
        ClientMsg::Chat {
            text,
            attachments,
            session_id,
            workspace,
        } => {
            let sid = SessionId::new(match session_id {
                Some(s) => s,
                None => uuid::Uuid::new_v4().to_string(),
            });

            // Subscribe to bus for this session
            let mut rx = match ctx
                .bus
                .subscribe(SubscriptionFilter {
                    session_ids: Some(vec![sid.clone()]),
                    recipients: None,
                    kinds: None,
                })
                .await
            {
                Ok(rx) => rx,
                Err(_) => return,
            };

            // Send JoinSession for the agent
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: sid.clone(),
                    sender: sylvander_agent::bus::Sender::System,
                    recipient: sylvander_agent::bus::Recipient::Agent(agent_id.clone()),
                    kind: MessageKind::System(SystemMessage::JoinSession {
                        session_id: sid.clone(),
                        metadata: sylvander_agent::session::SessionMetadata {
                            workspace: workspace
                                .map(std::path::PathBuf::from)
                                .unwrap_or_else(|| std::path::PathBuf::from("/tmp")),
                            name: "ws".into(),
                            user_id: "ws-client".into(),
                        },
                    }),
                    payload: String::new(),
                    attachments: Vec::new(),
                    timestamp: sylvander_agent::session::now_secs(),
                    id: sylvander_agent::bus::MessageId::new(),
                })
                .await;

            // Send user message
            let mut message = BusMessage::user_chat(sid.clone(), "ws-client", &text);
            message.attachments = attachments;
            let _ = ctx.bus.publish(message).await;

            // Notify client of session
            let _ = tx.send(ServerMsg::SessionCreated {
                session_id: sid.0.clone(),
                config: None,
            });

            // Stream events back to client until Done
            let tx_clone = tx.clone();
            tokio::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    if let MessageKind::Stream(ev) = msg.kind {
                        let s = &msg.session_id;
                        let out = match ev {
                            StreamEvent::TextDelta { delta } => Some(ServerMsg::TextDelta {
                                session_id: s.0.clone(),
                                delta,
                            }),
                            StreamEvent::ThinkingDelta { delta } => {
                                Some(ServerMsg::ThinkingDelta {
                                    session_id: s.0.clone(),
                                    delta,
                                })
                            }
                            StreamEvent::ToolCall {
                                call_id,
                                tool_name,
                                input,
                            } => Some(ServerMsg::ToolCall {
                                session_id: s.0.clone(),
                                call_id,
                                tool_name,
                                input,
                            }),
                            StreamEvent::ToolResult {
                                call_id,
                                tool_name,
                                output,
                                is_error,
                                ..
                            } => Some(ServerMsg::ToolResult {
                                session_id: s.0.clone(),
                                call_id,
                                tool_name,
                                output,
                                is_error,
                            }),
                            StreamEvent::IterationStart { iteration } => {
                                Some(ServerMsg::IterationStart {
                                    session_id: s.0.clone(),
                                    iteration,
                                })
                            }
                            StreamEvent::ToolApprovalRequired {
                                batch_id,
                                tools,
                                allowed_scopes,
                            } => Some(ServerMsg::ApprovalRequest {
                                session_id: s.0.clone(),
                                batch_id,
                                tools: tools
                                    .iter()
                                    .map(|t| ToolInfo {
                                        call_id: t.call_id.clone(),
                                        tool_name: t.tool_name.clone(),
                                        input: t.input.clone(),
                                    })
                                    .collect(),
                                allowed_scopes,
                            }),
                            StreamEvent::AskUser {
                                call_id,
                                question,
                                options,
                                multi_select,
                            } => Some(ServerMsg::AskUser {
                                session_id: s.0.clone(),
                                call_id,
                                question,
                                options,
                                multi_select,
                            }),
                            StreamEvent::UserAnswer { .. } => None,
                            StreamEvent::Done { text } => {
                                let _ = tx_clone.send(ServerMsg::Done {
                                    session_id: s.0.clone(),
                                    text,
                                });
                                break;
                            }
                            _ => None,
                        };
                        if let Some(m) = out {
                            let _ = tx_clone.send(m);
                        }
                    }
                }
            });
        }
        ClientMsg::Approve {
            session_id,
            call_id,
            approved,
            scope,
            reason,
        } => {
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: SessionId::new(session_id),
                    sender: sylvander_agent::bus::Sender::System,
                    recipient: sylvander_agent::bus::Recipient::Agent(agent_id.clone()),
                    kind: MessageKind::System(SystemMessage::ApproveTool {
                        call_id,
                        approved,
                        scope,
                        reason,
                    }),
                    payload: String::new(),
                    attachments: Vec::new(),
                    timestamp: sylvander_agent::session::now_secs(),
                    id: sylvander_agent::bus::MessageId::new(),
                })
                .await;
        }
        ClientMsg::Answer {
            session_id,
            call_id,
            answer,
        } => {
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: SessionId::new(session_id),
                    sender: sylvander_agent::bus::Sender::System,
                    recipient: sylvander_agent::bus::Recipient::Agent(agent_id.clone()),
                    kind: MessageKind::System(SystemMessage::AnswerQuestion { call_id, answer }),
                    payload: String::new(),
                    attachments: Vec::new(),
                    timestamp: sylvander_agent::session::now_secs(),
                    id: sylvander_agent::bus::MessageId::new(),
                })
                .await;
        }
        ClientMsg::DiscoverAgents => {
            if let Some(ui) = &ctx.ui {
                let _ = tx.send(ServerMsg::AgentsDiscovered {
                    agents: ui.discover_agents().await,
                });
            } else {
                operation_error(tx, "discover_agents", "UI service is unavailable");
            }
        }
        ClientMsg::CreateSession { request } => {
            if let Some(ui) = &ctx.ui {
                match ui.create_session(request).await {
                    Ok(config) => {
                        let _ = tx.send(ServerMsg::SessionCreated {
                            session_id: config.session_id.0.clone(),
                            config: Some(config),
                        });
                    }
                    Err(error) => operation_error(tx, "create_session", error),
                }
            } else {
                operation_error(tx, "create_session", "UI service is unavailable");
            }
        }
        ClientMsg::GetSessionConfig { session_id } => {
            if let Some(ui) = &ctx.ui {
                match ui.session_config(&SessionId::new(session_id)).await {
                    Ok(state) => {
                        let _ = tx.send(ServerMsg::SessionConfig { state });
                    }
                    Err(error) => operation_error(tx, "get_session_config", error),
                }
            } else {
                operation_error(tx, "get_session_config", "UI service is unavailable");
            }
        }
        ClientMsg::UpdateSessionConfig { request } => {
            if let Some(ui) = &ctx.ui {
                match ui.update_session_config(request).await {
                    Ok(state) => {
                        let _ = tx.send(ServerMsg::SessionConfig { state });
                    }
                    Err(error) => operation_error(tx, "update_session_config", error),
                }
            } else {
                operation_error(tx, "update_session_config", "UI service is unavailable");
            }
        }
        ClientMsg::SubmitFeedback { feedback } => {
            if let Some(ui) = &ctx.ui {
                match ui.submit_feedback(feedback).await {
                    Ok(feedback_id) => {
                        let _ = tx.send(ServerMsg::FeedbackRecorded { feedback_id });
                    }
                    Err(error) => operation_error(tx, "submit_feedback", error),
                }
            } else {
                operation_error(tx, "submit_feedback", "UI service is unavailable");
            }
        }
        ClientMsg::ListSessions => {
            info!("ws: client listed sessions (not yet fully implemented)");
        }
        ClientMsg::Ping => {
            let _ = tx.send(ServerMsg::Pong);
        }
        unsupported => {
            let _ = tx.send(ServerMsg::OperationError {
                operation: "websocket".into(),
                message: format!("operation is not supported by this transport: {unsupported:?}"),
            });
        }
    }
}

fn operation_error(
    tx: &mpsc::UnboundedSender<ServerMsg>,
    operation: &str,
    message: impl Into<String>,
) {
    let _ = tx.send(ServerMsg::OperationError {
        operation: operation.into(),
        message: message.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
