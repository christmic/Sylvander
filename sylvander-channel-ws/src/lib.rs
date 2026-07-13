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
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

use sylvander_agent::bus::{
    BusMessage, MessageKind, StreamEvent, SubscriptionFilter, SystemMessage,
};
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{Channel, ChannelContext};

// ===========================================================================
// Wire protocol
// ===========================================================================

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Chat {
        text: String,
        #[serde(default)]
        session_id: Option<String>,
    },
    Approve {
        call_id: String,
        approved: bool,
        #[serde(default)]
        scope: sylvander_agent::bus::ApprovalScope,
        #[serde(default)]
        reason: Option<String>,
    },
    /// User answered an AskUser question.
    Answer {
        call_id: String,
        answer: String,
    },
    ListSessions,
    Ping,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    SessionCreated {
        session_id: String,
    },
    TextDelta {
        session_id: String,
        delta: String,
    },
    ThinkingDelta {
        session_id: String,
        delta: String,
    },
    ToolCall {
        session_id: String,
        tool_name: String,
    },
    ToolResult {
        session_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    ToolApprovalRequired {
        session_id: String,
        batch_id: String,
        tools: Vec<ToolInfo>,
        allowed_scopes: Vec<sylvander_agent::bus::ApprovalScope>,
    },
    AskUser {
        session_id: String,
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },
    UserAnswer {
        session_id: String,
        call_id: String,
        answer: Vec<String>,
    },
    IterationStart {
        session_id: String,
        iteration: u32,
    },
    Done {
        session_id: String,
        text: String,
    },
    Error {
        session_id: String,
        message: String,
    },
    Pong,
}

#[derive(Debug, Clone, Serialize)]
struct ToolInfo {
    call_id: String,
    tool_name: String,
    input: serde_json::Value,
}

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

        // Spawn outgoing loop: bus events → fanout to all clients
        let bus_ctx = ctx.clone();
        let clients_out = clients.clone();
        tokio::spawn(async move { run_outgoing(bus_ctx, clients_out).await });

        // HTTP server
        let state = Arc::new(AppState {
            ctx,
            agent_id: self.agent_id.clone(),
            clients: clients.clone(),
            next_id: next_id.clone(),
        });

        let app = Router::new()
            .route("/ws", get(ws_handler))
            .with_state(state);

        let listener = tokio::net::TcpListener::bind(self.addr).await.unwrap();
        info!(addr = %self.addr, "ws channel listening");
        axum::serve(listener, app).await.unwrap();
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
        ClientMsg::Chat { text, session_id } => {
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
                            workspace: std::path::PathBuf::from("/tmp"),
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
            let _ = ctx
                .bus
                .publish(BusMessage::user_chat(sid.clone(), "ws-client", &text))
                .await;

            // Notify client of session
            let _ = tx.send(ServerMsg::SessionCreated {
                session_id: sid.0.clone(),
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
                            StreamEvent::ToolCall { tool_name, .. } => Some(ServerMsg::ToolCall {
                                session_id: s.0.clone(),
                                tool_name,
                            }),
                            StreamEvent::ToolResult {
                                tool_name,
                                output,
                                is_error,
                                ..
                            } => Some(ServerMsg::ToolResult {
                                session_id: s.0.clone(),
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
                            } => Some(ServerMsg::ToolApprovalRequired {
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
                            StreamEvent::UserAnswer { call_id, answer } => {
                                Some(ServerMsg::UserAnswer {
                                    session_id: s.0.clone(),
                                    call_id,
                                    answer,
                                })
                            }
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
            call_id,
            approved,
            scope,
            reason,
        } => {
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: SessionId::new(String::new()),
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
        ClientMsg::Answer { call_id, answer } => {
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: SessionId::new(String::new()),
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
        ClientMsg::ListSessions => {
            info!("ws: client listed sessions (not yet fully implemented)");
        }
        ClientMsg::Ping => {
            let _ = tx.send(ServerMsg::Pong);
        }
    }
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

// ===========================================================================
// Outgoing: bus events → all clients
// ===========================================================================

async fn run_outgoing(
    ctx: Arc<ChannelContext>,
    clients: Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<ServerMsg>>>>,
) {
    let mut rx = match ctx.bus.subscribe(SubscriptionFilter::all()).await {
        Ok(rx) => rx,
        Err(_) => return,
    };

    while let Some(msg) = rx.recv().await {
        let MessageKind::Stream(ref ev) = msg.kind else {
            continue;
        };

        let s = msg.session_id.0.clone();
        let server_msg = match ev {
            StreamEvent::TextDelta { delta } => Some(ServerMsg::TextDelta {
                session_id: s,
                delta: delta.clone(),
            }),
            StreamEvent::ThinkingDelta { delta } => Some(ServerMsg::ThinkingDelta {
                session_id: s,
                delta: delta.clone(),
            }),
            StreamEvent::ToolCall { tool_name, .. } => Some(ServerMsg::ToolCall {
                session_id: s,
                tool_name: tool_name.clone(),
            }),
            StreamEvent::ToolResult {
                tool_name,
                output,
                is_error,
                ..
            } => Some(ServerMsg::ToolResult {
                session_id: s,
                tool_name: tool_name.clone(),
                output: output.clone(),
                is_error: *is_error,
            }),
            StreamEvent::IterationStart { iteration } => Some(ServerMsg::IterationStart {
                session_id: s,
                iteration: *iteration,
            }),
            StreamEvent::Done { text } => Some(ServerMsg::Done {
                session_id: s,
                text: text.clone(),
            }),
            StreamEvent::ToolApprovalRequired {
                batch_id,
                tools,
                allowed_scopes,
            } => Some(ServerMsg::ToolApprovalRequired {
                session_id: s,
                batch_id: batch_id.clone(),
                tools: tools
                    .iter()
                    .map(|t| ToolInfo {
                        call_id: t.call_id.clone(),
                        tool_name: t.tool_name.clone(),
                        input: t.input.clone(),
                    })
                    .collect(),
                allowed_scopes: allowed_scopes.clone(),
            }),
            StreamEvent::AskUser {
                call_id,
                question,
                options,
                multi_select,
            } => Some(ServerMsg::AskUser {
                session_id: s,
                call_id: call_id.clone(),
                question: question.clone(),
                options: options.clone(),
                multi_select: *multi_select,
            }),
            StreamEvent::UserAnswer { call_id, answer } => Some(ServerMsg::UserAnswer {
                session_id: s,
                call_id: call_id.clone(),
                answer: answer.clone(),
            }),
            _ => None,
        };

        if let Some(m) = server_msg {
            // Fanout to all clients
            let clients_guard = clients.lock().await;
            for (_, tx) in clients_guard.iter() {
                let _ = tx.send(m.clone());
            }
        }
    }
}
