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
        attachments: Vec<sylvander_protocol::MessageAttachment>,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        workspace: Option<String>,
    },
    Approve {
        call_id: String,
        approved: bool,
        #[serde(default)]
        scope: sylvander_protocol::ApprovalScope,
    },
    Answer {
        call_id: String,
        answer: String,
    },
    Interrupt {
        session_id: String,
    },
    ResolvePlan {
        plan_id: String,
        decision: sylvander_protocol::PlanDecision,
    },
    CancelTask {
        session_id: String,
        task_id: String,
    },
    ListSessions,
    LoadSession {
        session_id: String,
    },
    RenameSession {
        session_id: String,
        label: String,
    },
    ArchiveSession {
        session_id: String,
    },
    RestoreSession {
        session_id: String,
    },
    DeleteSession {
        session_id: String,
    },
    ForkSession {
        session_id: String,
    },
    GetRuntimeInfo,
    SelectModel {
        model: String,
        reasoning_effort: sylvander_protocol::ReasoningEffort,
    },
    SelectPermissions {
        profile: sylvander_protocol::PermissionProfile,
    },
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
    ModelRetry {
        session_id: String,
        attempt: u32,
        max_attempts: u32,
        delay_ms: u64,
        reason: String,
    },
    ToolCall {
        session_id: String,
        call_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    ToolOutputDelta {
        session_id: String,
        call_id: String,
        tool_name: String,
        delta: String,
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
        allowed_scopes: Vec<sylvander_protocol::ApprovalScope>,
    },
    AskUser {
        session_id: String,
        call_id: String,
        question: String,
        options: Vec<String>,
        multi_select: bool,
    },
    TurnInterrupted {
        session_id: String,
        reason: String,
    },
    PlanProposed {
        session_id: String,
        plan_id: String,
        steps: Vec<String>,
        current: usize,
    },
    PlanUpdated {
        session_id: String,
        plan_id: String,
        steps: Vec<String>,
        current: usize,
    },
    TaskStarted {
        session_id: String,
        task_id: String,
        owner: String,
        purpose: String,
    },
    TaskProgress {
        session_id: String,
        task_id: String,
        message: String,
    },
    TaskCompleted {
        session_id: String,
        task_id: String,
        summary: String,
    },
    TaskFailed {
        session_id: String,
        task_id: String,
        error: String,
    },
    TaskCancelled {
        session_id: String,
        task_id: String,
        reason: String,
    },
    SessionsList {
        sessions: Vec<SessionInfo>,
    },
    SessionHistory {
        session: SessionInfo,
        messages: Vec<HistoryMessage>,
        iterations: u32,
        input_tokens: u64,
        output_tokens: u64,
    },
    SessionUpdated {
        session_id: String,
        label: Option<String>,
        archived: bool,
    },
    SessionDeleted {
        session_id: String,
    },
    RuntimeInfo {
        model: String,
        reasoning_effort: sylvander_protocol::ReasoningEffort,
        models: Vec<sylvander_protocol::ModelDescriptor>,
        permissions: sylvander_protocol::PermissionProfile,
        capabilities: u8,
        approval_enabled: bool,
        max_attachment_bytes: usize,
    },
    OperationError {
        operation: String,
        message: String,
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

#[derive(Debug, Serialize)]
struct HistoryMessage {
    role: String,
    text: String,
}

// ===========================================================================
// Channel
// ===========================================================================

pub struct UnixChannel {
    socket_path: PathBuf,
    agent_id: AgentId,
    runtime: RuntimeInfo,
    runtime_control: Option<sylvander_agent::run::AgentRun>,
}

#[derive(Clone)]
pub struct RuntimeInfo {
    pub model: String,
    pub reasoning_effort: sylvander_protocol::ReasoningEffort,
    pub models: Vec<sylvander_protocol::ModelDescriptor>,
    pub permissions: sylvander_protocol::PermissionProfile,
    pub capabilities: u8,
    pub approval_enabled: bool,
    pub max_attachment_bytes: usize,
}

impl UnixChannel {
    pub fn new(socket_path: impl Into<PathBuf>, agent_id: impl Into<AgentId>) -> Self {
        Self {
            socket_path: socket_path.into(),
            agent_id: agent_id.into(),
            runtime: RuntimeInfo {
                model: "unknown".into(),
                reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
                models: Vec::new(),
                permissions: sylvander_protocol::PermissionProfile::default(),
                capabilities: 0,
                approval_enabled: false,
                max_attachment_bytes: 512 * 1024,
            },
            runtime_control: None,
        }
    }

    pub fn with_runtime_info(mut self, runtime: RuntimeInfo) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn with_runtime_control(mut self, run: sylvander_agent::run::AgentRun) -> Self {
        self.runtime_control = Some(run);
        self
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
            let runtime = self.runtime.clone();
            let runtime_control = self.runtime_control.clone();
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
                    handle_client_msg(
                        msg,
                        &ctx_clone,
                        &agent_id_clone,
                        &tx,
                        &runtime,
                        runtime_control.as_ref(),
                    )
                    .await;
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
    runtime: &RuntimeInfo,
    runtime_control: Option<&sylvander_agent::run::AgentRun>,
) {
    match msg {
        ClientMsg::Chat {
            text,
            attachments,
            session_id,
            workspace,
        } => {
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
            let workspace = workspace
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            let session_name = workspace
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("Sylvander session")
                .to_string();
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: sid.clone(),
                    sender: sylvander_agent::bus::Sender::System,
                    recipient: sylvander_agent::bus::Recipient::Agent(agent_id.clone()),
                    kind: MessageKind::System(SystemMessage::JoinSession {
                        session_id: sid.clone(),
                        metadata: sylvander_agent::session::SessionMetadata {
                            workspace,
                            name: session_name,
                            user_id: "unix-client".into(),
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
                .publish(BusMessage::user_chat_with_attachments(
                    sid.clone(),
                    "unix-client",
                    &text,
                    attachments,
                ))
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
                            StreamEvent::ModelRetry {
                                attempt,
                                max_attempts,
                                delay_ms,
                                reason,
                            } => Some(ServerMsg::ModelRetry {
                                session_id: s.0.clone(),
                                attempt,
                                max_attempts,
                                delay_ms,
                                reason,
                            }),
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
                            StreamEvent::ToolOutputDelta {
                                call_id,
                                tool_name,
                                delta,
                            } => Some(ServerMsg::ToolOutputDelta {
                                session_id: s.0.clone(),
                                call_id,
                                tool_name,
                                delta,
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
                            StreamEvent::TurnInterrupted { reason } => {
                                Some(ServerMsg::TurnInterrupted {
                                    session_id: s.0.clone(),
                                    reason,
                                })
                            }
                            StreamEvent::PlanProposed {
                                plan_id,
                                steps,
                                current,
                            } => Some(ServerMsg::PlanProposed {
                                session_id: s.0.clone(),
                                plan_id,
                                steps,
                                current,
                            }),
                            StreamEvent::PlanUpdated {
                                plan_id,
                                steps,
                                current,
                            } => Some(ServerMsg::PlanUpdated {
                                session_id: s.0.clone(),
                                plan_id,
                                steps,
                                current,
                            }),
                            StreamEvent::TaskStarted {
                                task_id,
                                owner,
                                purpose,
                            } => Some(ServerMsg::TaskStarted {
                                session_id: s.0.clone(),
                                task_id,
                                owner,
                                purpose,
                            }),
                            StreamEvent::TaskProgress { task_id, message } => {
                                Some(ServerMsg::TaskProgress {
                                    session_id: s.0.clone(),
                                    task_id,
                                    message,
                                })
                            }
                            StreamEvent::TaskCompleted { task_id, summary } => {
                                Some(ServerMsg::TaskCompleted {
                                    session_id: s.0.clone(),
                                    task_id,
                                    summary,
                                })
                            }
                            StreamEvent::TaskFailed { task_id, error } => {
                                Some(ServerMsg::TaskFailed {
                                    session_id: s.0.clone(),
                                    task_id,
                                    error,
                                })
                            }
                            StreamEvent::TaskCancelled { task_id, reason } => {
                                Some(ServerMsg::TaskCancelled {
                                    session_id: s.0.clone(),
                                    task_id,
                                    reason,
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
        } => {
            // Forward approval to bus for any waiting agent
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: SessionId::new("").into(), // agent-level
                    sender: sylvander_agent::bus::Sender::System,
                    recipient: sylvander_agent::bus::Recipient::Agent(agent_id.clone()),
                    kind: MessageKind::System(SystemMessage::ApproveTool {
                        call_id,
                        approved,
                        scope,
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
                    session_id: SessionId::new("").into(),
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
        ClientMsg::Interrupt { session_id } => {
            let session_id = SessionId::new(session_id);
            let _ = ctx
                .bus
                .publish(BusMessage::system_interrupt_turn(
                    agent_id.clone(),
                    session_id,
                ))
                .await;
        }
        ClientMsg::ResolvePlan { plan_id, decision } => {
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: SessionId::new(""),
                    sender: sylvander_agent::bus::Sender::System,
                    recipient: sylvander_agent::bus::Recipient::Agent(agent_id.clone()),
                    kind: MessageKind::System(SystemMessage::ResolvePlan { plan_id, decision }),
                    payload: String::new(),
                    attachments: Vec::new(),
                    timestamp: sylvander_agent::session::now_secs(),
                    id: sylvander_agent::bus::MessageId::new(),
                })
                .await;
        }
        ClientMsg::CancelTask {
            session_id,
            task_id,
        } => {
            let session_id = SessionId::new(session_id);
            let _ = ctx
                .bus
                .publish(BusMessage {
                    session_id: session_id.clone(),
                    sender: sylvander_agent::bus::Sender::System,
                    recipient: sylvander_agent::bus::Recipient::Agent(agent_id.clone()),
                    kind: MessageKind::System(SystemMessage::CancelTask {
                        session_id,
                        task_id,
                    }),
                    payload: String::new(),
                    attachments: Vec::new(),
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
                            last_seen_secs: u64::try_from(now.saturating_sub(session.updated_at))
                                .unwrap_or(0),
                        })
                        .collect();
                    let _ = tx.send(ServerMsg::SessionsList { sessions });
                }
                Err(error) => {
                    warn!(error = %error, "unix: failed to list sessions");
                    operation_error(tx, "list_sessions", error.to_string());
                }
            }
        }
        ClientMsg::LoadSession { session_id } => {
            let session_id = SessionId::new(session_id);
            let caller = unix_session_context(agent_id, session_id.clone());
            match ctx.sessions.get(&session_id).await {
                Ok(Some(session)) => match ctx
                    .sessions
                    .read_history(&caller, &session_id, true, None)
                    .await
                {
                    Ok(messages) => {
                        let messages = messages
                            .into_iter()
                            .filter_map(|message| {
                                history_text(&message.content).map(|text| HistoryMessage {
                                    role: match message.role {
                                        sylvander_agent::session_store::MessageRole::User => "user",
                                        sylvander_agent::session_store::MessageRole::Assistant => {
                                            "assistant"
                                        }
                                        sylvander_agent::session_store::MessageRole::Tool => "tool",
                                    }
                                    .into(),
                                    text,
                                })
                            })
                            .collect();
                        let usage = ctx.sessions.usage(&session_id).await.unwrap_or_default();
                        let _ = tx.send(ServerMsg::SessionHistory {
                            session: session_info(session),
                            messages,
                            iterations: usage.iterations,
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                        });
                    }
                    Err(error) => warn!(%error, "unix: failed to load session history"),
                },
                Ok(None) => operation_error(tx, "load_session", "session not found"),
                Err(error) => {
                    warn!(%error, "unix: failed to get session");
                    operation_error(tx, "load_session", error.to_string());
                }
            }
        }
        ClientMsg::RenameSession { session_id, label } => {
            let session_id = SessionId::new(session_id);
            match ctx.sessions.get(&session_id).await {
                Ok(Some(mut session)) => {
                    session.name = label.clone();
                    session.metadata.name = label.clone();
                    match ctx.sessions.save(&session).await {
                        Ok(()) => {
                            let _ = tx.send(ServerMsg::SessionUpdated {
                                session_id: session_id.0,
                                label: Some(label),
                                archived: false,
                            });
                        }
                        Err(error) => warn!(%error, "unix: failed to rename session"),
                    }
                }
                Ok(None) => warn!(%session_id, "unix: rename session not found"),
                Err(error) => warn!(%error, "unix: failed to get session for rename"),
            }
        }
        ClientMsg::ArchiveSession { session_id } => {
            let session_id = SessionId::new(session_id);
            match ctx.sessions.archive(&session_id).await {
                Ok(()) => {
                    let _ = tx.send(ServerMsg::SessionUpdated {
                        session_id: session_id.0,
                        label: None,
                        archived: true,
                    });
                }
                Err(error) => warn!(%error, "unix: failed to archive session"),
            }
        }
        ClientMsg::RestoreSession { session_id } => {
            let session_id = SessionId::new(session_id);
            match ctx.sessions.restore(&session_id).await {
                Ok(()) => {
                    let _ = tx.send(ServerMsg::SessionUpdated {
                        session_id: session_id.0,
                        label: None,
                        archived: false,
                    });
                }
                Err(error) => warn!(%error, "unix: failed to restore session"),
            }
        }
        ClientMsg::DeleteSession { session_id } => {
            let session_id = SessionId::new(session_id);
            match ctx.sessions.delete(&session_id).await {
                Ok(()) => {
                    let _ = tx.send(ServerMsg::SessionDeleted {
                        session_id: session_id.0,
                    });
                }
                Err(error) => {
                    warn!(%error, "unix: failed to permanently delete session");
                    operation_error(tx, "delete_session", error.to_string());
                }
            }
        }
        ClientMsg::ForkSession { session_id } => {
            let source_id = SessionId::new(session_id);
            let caller = unix_session_context(agent_id, source_id.clone());
            match ctx.sessions.get(&source_id).await {
                Ok(Some(source)) => {
                    let fork_id = SessionId::new(uuid::Uuid::new_v4().to_string());
                    let mut fork = source.clone();
                    fork.id = fork_id.clone();
                    fork.name = format!("{} (fork)", source.name);
                    fork.metadata.name = fork.name.clone();
                    fork.created_at = sylvander_agent::session::now_secs();
                    fork.updated_at = fork.created_at;
                    if let Err(error) = ctx.sessions.save(&fork).await {
                        warn!(%error, "unix: failed to save forked session");
                        return;
                    }
                    let history = match ctx
                        .sessions
                        .read_history(&caller, &source_id, true, None)
                        .await
                    {
                        Ok(history) => history,
                        Err(error) => {
                            warn!(%error, "unix: failed to read source history for fork");
                            return;
                        }
                    };
                    let fork_caller = unix_session_context(agent_id, fork_id.clone());
                    for message in &history {
                        if let Err(error) = ctx
                            .sessions
                            .append_message(
                                &fork_caller,
                                &fork_id,
                                message.role,
                                message.content.clone(),
                                message.model_id.as_deref(),
                                message.tool_name.as_deref(),
                                None,
                            )
                            .await
                        {
                            warn!(%error, "unix: failed to copy fork history");
                            return;
                        }
                    }
                    let messages = history
                        .into_iter()
                        .filter_map(|message| {
                            history_text(&message.content).map(|text| HistoryMessage {
                                role: match message.role {
                                    sylvander_agent::session_store::MessageRole::User => "user",
                                    sylvander_agent::session_store::MessageRole::Assistant => {
                                        "assistant"
                                    }
                                    sylvander_agent::session_store::MessageRole::Tool => "tool",
                                }
                                .into(),
                                text,
                            })
                        })
                        .collect();
                    let _ = tx.send(ServerMsg::SessionHistory {
                        session: session_info(fork),
                        messages,
                        iterations: 0,
                        input_tokens: 0,
                        output_tokens: 0,
                    });
                }
                Ok(None) => warn!(%source_id, "unix: fork source not found"),
                Err(error) => warn!(%error, "unix: failed to get fork source"),
            }
        }
        ClientMsg::GetRuntimeInfo => {
            let model_info = if let Some(control) = runtime_control {
                control.runtime_model_info().await
            } else {
                sylvander_protocol::RuntimeModelInfo {
                    current_model: runtime.model.clone(),
                    reasoning_effort: runtime.reasoning_effort,
                    models: runtime.models.clone(),
                }
            };
            let permissions = if let Some(control) = runtime_control {
                control.permission_profile().await
            } else {
                runtime.permissions.clone()
            };
            let capabilities = model_info
                .models
                .iter()
                .find(|model| model.id == model_info.current_model)
                .map_or(runtime.capabilities, |model| model.capabilities);
            let _ = tx.send(ServerMsg::RuntimeInfo {
                model: model_info.current_model,
                reasoning_effort: model_info.reasoning_effort,
                models: model_info.models,
                permissions,
                capabilities,
                approval_enabled: runtime.approval_enabled,
                max_attachment_bytes: runtime.max_attachment_bytes,
            });
        }
        ClientMsg::SelectModel {
            model,
            reasoning_effort,
        } => {
            let Some(control) = runtime_control else {
                let _ = tx.send(ServerMsg::OperationError {
                    operation: "select_model".into(),
                    message: "runtime model control is unavailable".into(),
                });
                return;
            };
            match control.select_model(&model, reasoning_effort).await {
                Ok(model_info) => {
                    let capabilities = model_info
                        .models
                        .iter()
                        .find(|entry| entry.id == model_info.current_model)
                        .map_or(0, |entry| entry.capabilities);
                    let _ = tx.send(ServerMsg::RuntimeInfo {
                        model: model_info.current_model,
                        reasoning_effort: model_info.reasoning_effort,
                        models: model_info.models,
                        permissions: control.permission_profile().await,
                        capabilities,
                        approval_enabled: runtime.approval_enabled,
                        max_attachment_bytes: runtime.max_attachment_bytes,
                    });
                }
                Err(message) => {
                    let _ = tx.send(ServerMsg::OperationError {
                        operation: "select_model".into(),
                        message,
                    });
                }
            }
        }
        ClientMsg::SelectPermissions { profile } => {
            let Some(control) = runtime_control else {
                let _ = tx.send(ServerMsg::OperationError {
                    operation: "select_permissions".into(),
                    message: "runtime permission control is unavailable".into(),
                });
                return;
            };
            match control.select_permissions(profile).await {
                Ok(permissions) => {
                    let model_info = control.runtime_model_info().await;
                    let capabilities = model_info
                        .models
                        .iter()
                        .find(|entry| entry.id == model_info.current_model)
                        .map_or(0, |entry| entry.capabilities);
                    let _ = tx.send(ServerMsg::RuntimeInfo {
                        model: model_info.current_model,
                        reasoning_effort: model_info.reasoning_effort,
                        models: model_info.models,
                        permissions,
                        capabilities,
                        approval_enabled: runtime.approval_enabled,
                        max_attachment_bytes: runtime.max_attachment_bytes,
                    });
                }
                Err(message) => {
                    let _ = tx.send(ServerMsg::OperationError {
                        operation: "select_permissions".into(),
                        message,
                    });
                }
            }
        }
        ClientMsg::Ping => {
            let _ = tx.send(ServerMsg::Pong);
        }
    }
}

fn unix_session_context(
    agent_id: &AgentId,
    session_id: SessionId,
) -> sylvander_protocol::SessionContext {
    sylvander_protocol::SessionContext::new("unix-client", agent_id.clone(), session_id)
}

fn session_info(session: sylvander_agent::session_store::StoredSession) -> SessionInfo {
    let now = sylvander_agent::session::now_secs();
    SessionInfo {
        id: session.id.0,
        label: if session.name.is_empty() {
            "untitled session".into()
        } else {
            session.name
        },
        workspace: session.metadata.workspace.display().to_string(),
        last_seen_secs: u64::try_from(now.saturating_sub(session.updated_at)).unwrap_or(0),
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

fn history_text(value: &serde_json::Value) -> Option<String> {
    let content = value.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let text = content
        .as_array()?
        .iter()
        .filter_map(|block| {
            block
                .get("text")
                .and_then(serde_json::Value::as_str)
                .or_else(|| block.get("content").and_then(serde_json::Value::as_str))
        })
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_agent::bus::{InProcessMessageBus, MessageBus};
    use sylvander_agent::session_store::{
        MessageRole, SessionLifetime, SessionStore, SqliteSessionStore, StoredSession,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn socket_path() -> PathBuf {
        PathBuf::from("/tmp").join(format!(
            "sylv-u-{}-{}.sock",
            std::process::id(),
            &uuid::Uuid::new_v4().to_string()[..8]
        ))
    }

    fn runtime_info() -> RuntimeInfo {
        RuntimeInfo {
            model: "test-model".into(),
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
            models: vec![sylvander_protocol::ModelDescriptor {
                id: "test-model".into(),
                provider: "test".into(),
                capabilities: 0b101,
                reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
            }],
            permissions: sylvander_protocol::PermissionProfile::default(),
            capabilities: 0b101,
            approval_enabled: true,
            max_attachment_bytes: 1024,
        }
    }

    async fn connect(path: &std::path::Path) -> tokio::net::UnixStream {
        for _ in 0..40 {
            if let Ok(stream) = tokio::net::UnixStream::connect(path).await {
                return stream;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("unix channel did not start");
    }

    async fn send_and_read(
        write: &mut tokio::net::unix::OwnedWriteHalf,
        reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
        message: serde_json::Value,
    ) -> serde_json::Value {
        write
            .write_all(format!("{message}\n").as_bytes())
            .await
            .expect("write");
        let line = tokio::time::timeout(std::time::Duration::from_secs(1), reader.next_line())
            .await
            .expect("response timeout")
            .expect("read")
            .expect("response");
        serde_json::from_str(&line).expect("json response")
    }

    #[tokio::test]
    async fn runtime_info_reports_server_truth() {
        let bus = Arc::new(InProcessMessageBus::new());
        let context = ChannelContext {
            bus,
            sessions: Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::GetRuntimeInfo,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
            None,
        )
        .await;

        let response = rx.recv().await.expect("runtime response");
        assert!(matches!(
            response,
            ServerMsg::RuntimeInfo {
                model,
                reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
                models,
                permissions: sylvander_protocol::PermissionProfile {
                    file_access: sylvander_protocol::FileAccess::WorkspaceWrite,
                    network_access: sylvander_protocol::NetworkAccess::Denied,
                    approval_policy: sylvander_protocol::ApprovalPolicy::Allow,
                },
                capabilities: 0b101,
                approval_enabled: true,
                max_attachment_bytes: 1024,
            } if model == "test-model" && models.len() == 1
        ));
    }

    #[tokio::test]
    async fn model_selection_is_acknowledged_from_agent_runtime_truth() {
        use sylvander_llm_anthropic::api::client::AnthropicClient;
        use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};

        let bus = Arc::new(InProcessMessageBus::new());
        let spec = sylvander_agent::spec::AgentSpec::builder()
            .id("agent-1")
            .name("Agent")
            .model_name("test-model")
            .build()
            .expect("spec");
        let client = AnthropicClient::builder()
            .api_key("test")
            .build()
            .expect("client");
        let thinking = ModelInfo::builder()
            .id("thinking-model")
            .context_window(200_000)
            .max_output_tokens(32_000)
            .capability(ModelCapabilities::EXTENDED_THINKING)
            .build()
            .expect("model");
        let run = sylvander_agent::run::AgentRun::builder(spec, client)
            .bus(bus.clone())
            .available_models(vec![thinking])
            .build()
            .expect("run");
        let context = ChannelContext {
            bus,
            sessions: Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::SelectModel {
                model: "thinking-model".into(),
                reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
            Some(&run),
        )
        .await;

        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::RuntimeInfo {
                model,
                reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
                ..
            }) if model == "thinking-model"
        ));

        handle_client_msg(
            ClientMsg::SelectPermissions {
                profile: sylvander_protocol::PermissionProfile {
                    file_access: sylvander_protocol::FileAccess::ReadOnly,
                    network_access: sylvander_protocol::NetworkAccess::Denied,
                    approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
                },
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
            Some(&run),
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::RuntimeInfo {
                permissions: sylvander_protocol::PermissionProfile {
                    file_access: sylvander_protocol::FileAccess::ReadOnly,
                    approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
                    ..
                },
                ..
            })
        ));
    }

    #[tokio::test]
    async fn persisted_session_load_rename_fork_and_archive_round_trip() {
        let path = socket_path();
        let agent_id = AgentId::new("agent-1");
        let store: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store"));
        let session_id = SessionId::new("session-1");
        let metadata = sylvander_agent::session::SessionMetadata {
            workspace: "/workspace/project".into(),
            name: "Original".into(),
            user_id: "unix-client".into(),
        };
        store
            .save(&StoredSession::new(
                session_id.clone(),
                "Original",
                SessionLifetime::Persistent,
                metadata,
                vec![agent_id.clone()],
            ))
            .await
            .expect("save");
        let caller = unix_session_context(&agent_id, session_id.clone());
        store
            .append_message(
                &caller,
                &session_id,
                MessageRole::User,
                serde_json::json!({"role":"user","content":"hello"}),
                None,
                None,
                None,
            )
            .await
            .expect("append");
        store
            .record_usage(&session_id, 120, 30)
            .await
            .expect("usage");

        let channel = Arc::new(UnixChannel::new(&path, agent_id));
        let context = ChannelContext {
            bus: Arc::new(InProcessMessageBus::new()),
            sessions: store.clone(),
        };
        let task = tokio::spawn(channel.run(context));
        let stream = connect(&path).await;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        let loaded = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({"type":"load_session","session_id":"session-1"}),
        )
        .await;
        assert_eq!(loaded["type"], "session_history");
        assert_eq!(loaded["messages"][0]["text"], "hello");
        assert_eq!(loaded["iterations"], 1);
        assert_eq!(loaded["input_tokens"], 120);
        assert_eq!(loaded["output_tokens"], 30);

        let renamed = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({
                "type":"rename_session",
                "session_id":"session-1",
                "label":"Renamed"
            }),
        )
        .await;
        assert_eq!(renamed["label"], "Renamed");

        let forked = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({"type":"fork_session","session_id":"session-1"}),
        )
        .await;
        assert_eq!(forked["type"], "session_history");
        assert_ne!(forked["session"]["id"], "session-1");
        assert_eq!(forked["messages"][0]["text"], "hello");

        let archived = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({"type":"archive_session","session_id":"session-1"}),
        )
        .await;
        assert_eq!(archived["archived"], true);

        let restored = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({"type":"restore_session","session_id":"session-1"}),
        )
        .await;
        assert_eq!(restored["archived"], false);
        let loaded_again = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({"type":"load_session","session_id":"session-1"}),
        )
        .await;
        assert_eq!(loaded_again["messages"][0]["text"], "hello");

        let missing = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({"type":"load_session","session_id":"missing"}),
        )
        .await;
        assert_eq!(missing["type"], "operation_error");
        assert_eq!(missing["operation"], "load_session");

        let deleted = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({"type":"delete_session","session_id":"session-1"}),
        )
        .await;
        assert_eq!(deleted["type"], "session_deleted");
        assert_eq!(deleted["session_id"], "session-1");

        task.abort();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn typed_plan_resolution_is_forwarded_to_the_agent_bus() {
        let bus = Arc::new(InProcessMessageBus::new());
        let agent_id = AgentId::new("agent-1");
        let mut inbox = bus
            .subscribe(SubscriptionFilter::for_agent(agent_id.clone()))
            .await
            .expect("subscribe");
        let context = ChannelContext {
            bus,
            sessions: Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        };
        let (tx, _rx) = mpsc::unbounded_channel();

        handle_client_msg(
            ClientMsg::ResolvePlan {
                plan_id: "plan-1".into(),
                decision: sylvander_protocol::PlanDecision::Revised {
                    steps: vec!["inspect".into(), "verify".into()],
                },
            },
            &context,
            &agent_id,
            &tx,
            &runtime_info(),
            None,
        )
        .await;

        let message = inbox.recv().await.expect("agent message");
        assert!(matches!(
            message.kind,
            MessageKind::System(SystemMessage::ResolvePlan {
                plan_id,
                decision: sylvander_protocol::PlanDecision::Revised { steps },
            }) if plan_id == "plan-1" && steps == ["inspect", "verify"]
        ));
    }

    #[tokio::test]
    async fn approval_scope_is_forwarded_without_transport_interpretation() {
        let bus = Arc::new(InProcessMessageBus::new());
        let agent_id = AgentId::new("agent-1");
        let mut inbox = bus
            .subscribe(SubscriptionFilter::for_agent(agent_id.clone()))
            .await
            .expect("subscribe");
        let context = ChannelContext {
            bus,
            sessions: Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::Approve {
                call_id: "call-1".into(),
                approved: true,
                scope: sylvander_protocol::ApprovalScope::Session,
            },
            &context,
            &agent_id,
            &tx,
            &runtime_info(),
            None,
        )
        .await;

        let message = inbox.recv().await.expect("agent message");
        assert!(matches!(
            message.kind,
            MessageKind::System(SystemMessage::ApproveTool {
                call_id,
                approved: true,
                scope: sylvander_protocol::ApprovalScope::Session,
            }) if call_id == "call-1"
        ));
    }

    #[tokio::test]
    async fn task_cancel_preserves_session_scope_on_the_agent_bus() {
        let bus = Arc::new(InProcessMessageBus::new());
        let agent_id = AgentId::new("agent-1");
        let mut inbox = bus
            .subscribe(SubscriptionFilter::for_agent(agent_id.clone()))
            .await
            .expect("subscribe");
        let context = ChannelContext {
            bus,
            sessions: Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::CancelTask {
                session_id: "session-1".into(),
                task_id: "task-1".into(),
            },
            &context,
            &agent_id,
            &tx,
            &runtime_info(),
            None,
        )
        .await;

        let message = inbox.recv().await.expect("agent message");
        assert!(matches!(
            message.kind,
            MessageKind::System(SystemMessage::CancelTask { session_id, task_id })
                if session_id.0 == "session-1" && task_id == "task-1"
        ));
    }

    #[tokio::test]
    async fn chat_forwards_typed_attachments_without_flattening() {
        let bus = Arc::new(InProcessMessageBus::new());
        let mut events = bus
            .subscribe(SubscriptionFilter::all())
            .await
            .expect("subscribe");
        let agent_id = AgentId::new("agent-1");
        let context = ChannelContext {
            bus,
            sessions: Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
        };
        let (tx, _rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::Chat {
                text: "review".into(),
                attachments: vec![sylvander_protocol::MessageAttachment {
                    id: "a1".into(),
                    kind: sylvander_protocol::AttachmentKind::File,
                    name: "src/main.rs".into(),
                    mime_type: "text/x-rust".into(),
                    content: sylvander_protocol::AttachmentContent::Text {
                        text: "fn main() {}".into(),
                    },
                    byte_count: 12,
                }],
                session_id: Some("session-1".into()),
                workspace: Some("/repo".into()),
            },
            &context,
            &agent_id,
            &tx,
            &runtime_info(),
            None,
        )
        .await;

        let chat = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let message = events.recv().await.expect("bus event");
                if matches!(message.kind, MessageKind::Chat) {
                    break message;
                }
            }
        })
        .await
        .expect("chat");
        assert_eq!(chat.attachments.len(), 1);
        assert_eq!(chat.attachments[0].name, "src/main.rs");
    }
}
