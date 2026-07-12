//! Unix socket channel — line-based JSON protocol over UDS.
//!
//! # Protocol
//!
//! One JSON object per line. Client sends commands; server pushes events.
//!
//! ## Client → Server
//! ```json
//! {"type":"chat","text":"hello"}
//! {"type":"chat","text":"hi","session_id":"abc"}
//! {"type":"approve","call_id":"toolu_001","approved":true}
//! {"type":"list_sessions"}
//! {"type":"ping"}
//! ```
//!
//! ## Server → Client (pushed as StreamEvent)
//! ```json
//! {"type":"text_delta","session_id":"...","delta":"..."}
//! {"type":"tool_call","session_id":"...","tool_name":"..."}
//! {"type":"tool_result","session_id":"...","tool_name":"...","output":"...","is_error":false}
//! {"type":"tool_rejected","session_id":"...","tool_name":"...","reason":"..."}
//! {"type":"iteration_start","session_id":"...","iteration":1}
//! {"type":"done","session_id":"...","text":"..."}
//! {"type":"error","session_id":"...","message":"..."}
//! {"type":"approval_request","session_id":"...","batch_id":"...","tools":[...]}
//! {"type":"session_created","session_id":"..."}
//! {"type":"pong"}
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
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
    },
    Answer {
        call_id: String,
        answer: String,
    },
    ListSessions,
    Ping,
}

#[derive(Debug, Serialize)]
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
        call_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    ToolResult {
        session_id: String,
        call_id: String,
        tool_name: String,
        output: String,
        is_error: bool,
    },
    IterationStart {
        session_id: String,
        iteration: u32,
    },
    IterationEnd {
        session_id: String,
        iteration: u32,
        input_tokens: u32,
        output_tokens: u32,
    },
    Done {
        session_id: String,
        text: String,
    },
    Error {
        session_id: String,
        message: String,
    },
    ApprovalRequest {
        session_id: String,
        batch_id: String,
        tools: Vec<ToolInfo>,
    },
    AskUser {
        session_id: String,
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },
    SessionsList {
        sessions: Vec<SessionInfo>,
    },
    Pong,
}

#[derive(Debug, Serialize)]
struct ToolInfo {
    call_id: String,
    tool_name: String,
    input: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct SessionInfo {
    id: String,
    label: String,
    workspace: String,
    last_seen_secs: u64,
}

// ===========================================================================
// Channel
// ===========================================================================

pub struct UnixChannel {
    socket_path: PathBuf,
    agent_id: AgentId,
}

impl UnixChannel {
    pub fn new(socket_path: impl Into<PathBuf>, agent_id: impl Into<AgentId>) -> Self {
        Self {
            socket_path: socket_path.into(),
            agent_id: agent_id.into(),
        }
    }
}

#[async_trait]
impl Channel for UnixChannel {
    fn name(&self) -> &str {
        "unix"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);
        let agent_id = self.agent_id.clone();

        // Remove stale socket file
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = match tokio::net::UnixListener::bind(&self.socket_path) {
            Ok(l) => {
                info!(path = ?self.socket_path, "unix channel listening");
                l
            }
            Err(e) => {
                warn!(error = %e, "unix: bind failed");
                return;
            }
        };

        // Active clients: client_id → tx
        let clients: Arc<Mutex<HashMap<u64, mpsc::UnboundedSender<ServerMsg>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let next_id: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));

        loop {
            let (stream, _addr) = match listener.accept().await {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "unix: accept failed");
                    continue;
                }
            };

            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read).lines();

            let client_id = {
                let mut id = next_id.lock().await;
                *id += 1;
                *id
            };

            let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
            clients.lock().await.insert(client_id, tx.clone());

            // Spawn writer task
            let clients_writer = clients.clone();
            tokio::spawn(async move {
                while let Some(msg) = rx.recv().await {
                    if let Ok(s) = serde_json::to_string(&msg) {
                        let _ = write.write_all(s.as_bytes()).await;
                        let _ = write.write_all(b"\n").await;
                    }
                }
                // Client disconnected — clean up
                clients_writer.lock().await.remove(&client_id);
            });

            // Spawn reader task
            let ctx_clone = ctx.clone();
            let agent_id_clone = agent_id.clone();
            let clients_clean = clients.clone();
            let client_id_clone = client_id;
            tokio::spawn(async move {
                while let Ok(Some(line)) = reader.next_line().await {
                    let msg: ClientMsg = match serde_json::from_str(&line) {
                        Ok(m) => m,
                        Err(e) => {
                            warn!(error = %e, line = %line, "unix: bad json");
                            continue;
                        }
                    };
                    handle_client_msg(msg, &ctx_clone, &agent_id_clone, &tx).await;
                }
                // Client disconnected
                clients_clean.lock().await.remove(&client_id_clone);
            });
        }
    }
}

async fn handle_client_msg(
    msg: ClientMsg,
    ctx: &ChannelContext,
    agent_id: &AgentId,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    match msg {
        ClientMsg::Chat { text, session_id } => {
            let sid = match session_id {
                Some(s) => SessionId::new(s),
                None => SessionId::new(uuid::Uuid::new_v4().to_string()),
            };

            // Subscribe to bus for this session
            let mut rx = ctx
                .bus
                .subscribe(SubscriptionFilter {
                    session_ids: Some(vec![sid.clone()]),
                    recipients: None,
                    kinds: None,
                })
                .await
                .expect("subscribe");

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
                            workspace: PathBuf::from("/tmp"),
                            name: "unix".into(),
                            user_id: "unix-client".into(),
                        },
                    }),
                    payload: String::new(),
                    timestamp: sylvander_agent::session::now_secs(),
                    id: sylvander_agent::bus::MessageId::new(),
                })
                .await;

            // Send user message
            let _ = ctx
                .bus
                .publish(BusMessage::user_chat(sid.clone(), "unix-client", &text))
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
                            StreamEvent::IterationEnd {
                                iteration,
                                input_tokens,
                                output_tokens,
                            } => Some(ServerMsg::IterationEnd {
                                session_id: s.0.clone(),
                                iteration,
                                input_tokens,
                                output_tokens,
                            }),
                            StreamEvent::ToolApprovalRequired { batch_id, tools } => {
                                Some(ServerMsg::ApprovalRequest {
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
                                })
                            }
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
        ClientMsg::Approve { call_id, approved } => {
            // Forward approval to bus for any waiting agent
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: SessionId::new("").into(), // agent-level
                    sender: sylvander_agent::bus::Sender::System,
                    recipient: sylvander_agent::bus::Recipient::Agent(agent_id.clone()),
                    kind: MessageKind::System(SystemMessage::ApproveTool { call_id, approved }),
                    payload: String::new(),
                    timestamp: sylvander_agent::session::now_secs(),
                    id: sylvander_agent::bus::MessageId::new(),
                })
                .await;
        }
        ClientMsg::Answer { call_id, answer } => {
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: SessionId::new("").into(),
                    sender: sylvander_agent::bus::Sender::System,
                    recipient: sylvander_agent::bus::Recipient::Agent(agent_id.clone()),
                    kind: MessageKind::System(SystemMessage::AnswerQuestion { call_id, answer }),
                    payload: String::new(),
                    timestamp: sylvander_agent::session::now_secs(),
                    id: sylvander_agent::bus::MessageId::new(),
                })
                .await;
        }
        ClientMsg::ListSessions => {
            let caller = sylvander_protocol::SessionContext::new(
                "unix-client",
                agent_id.clone(),
                "__session_list__",
            );
            let filter = sylvander_agent::session_store::SessionFilter {
                identity: Some(caller.identity.clone()),
                limit: Some(100),
                ..Default::default()
            };
            match ctx.sessions.list(&caller, filter).await {
                Ok(sessions) => {
                    let now = sylvander_agent::session::now_secs();
                    let sessions = sessions
                        .into_iter()
                        .map(|session| SessionInfo {
                            id: session.id.0,
                            label: if session.name.is_empty() {
                                "untitled session".into()
                            } else {
                                session.name
                            },
                            workspace: session.metadata.workspace.display().to_string(),
                            last_seen_secs: u64::try_from(now.saturating_sub(session.created_at))
                                .unwrap_or(0),
                        })
                        .collect();
                    let _ = tx.send(ServerMsg::SessionsList { sessions });
                }
                Err(error) => warn!(error = %error, "unix: failed to list sessions"),
            }
        }
        ClientMsg::Ping => {
            let _ = tx.send(ServerMsg::Pong);
        }
    }
}
