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
//! ## Server → Client (pushed as `StreamEvent`)
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

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, mpsc};
use tokio_util::codec::{FramedRead, LinesCodec};
use tracing::{info, warn};

use sylvander_agent::bus::{MessageKind, StreamEvent};
use sylvander_agent::session_store::SessionMetadataPatch;
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{
    Channel, ChannelContext, ExternalChatRequest, submit_external_chat,
    unavailable_agent_admin_response, unavailable_registry_admin_response,
};
use sylvander_protocol::{
    SessionConfigOverrides, SessionWorkspaceBinding, UiClientMessage as ClientMsg,
    UiHistoryMessage as HistoryMessage, UiServerMessage as ServerMsg, UiSessionInfo as SessionInfo,
    UiToolInfo as ToolInfo,
};

// ===========================================================================
// Wire protocol
// ===========================================================================

#[derive(Default)]
struct SessionReplay {
    active: bool,
    events: Vec<ServerMsg>,
    bytes: usize,
    truncated: bool,
}

#[derive(Default)]
struct RelayHub {
    clients: HashMap<u64, mpsc::UnboundedSender<ServerMsg>>,
    session_clients: HashMap<SessionId, HashSet<u64>>,
    relays: HashSet<SessionId>,
    replay: HashMap<SessionId, SessionReplay>,
}

// ===========================================================================
// Channel
// ===========================================================================

pub struct UnixChannel {
    socket_path: PathBuf,
    instance_id: String,
    agent_id: AgentId,
    runtime: RuntimeInfo,
    max_request_bytes: usize,
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
    pub platform: sylvander_protocol::PlatformSnapshot,
}

impl UnixChannel {
    pub fn new(socket_path: impl Into<PathBuf>, agent_id: impl Into<AgentId>) -> Self {
        Self {
            socket_path: socket_path.into(),
            instance_id: "unix".into(),
            agent_id: agent_id.into(),
            runtime: RuntimeInfo {
                model: "unknown".into(),
                reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
                models: Vec::new(),
                permissions: sylvander_protocol::PermissionProfile::default(),
                capabilities: 0,
                approval_enabled: false,
                max_attachment_bytes: 512 * 1024,
                platform: sylvander_protocol::PlatformSnapshot::default(),
            },
            max_request_bytes: 1024 * 1024,
        }
    }

    pub fn with_instance_id(mut self, instance_id: impl Into<String>) -> Self {
        self.instance_id = instance_id.into();
        self
    }

    pub fn with_runtime_info(mut self, runtime: RuntimeInfo) -> Self {
        self.runtime = runtime;
        self
    }

    #[must_use]
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }
}

#[async_trait]
impl Channel for UnixChannel {
    fn name(&self) -> &'static str {
        "unix"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);
        let agent_id = self.agent_id.clone();

        // Remove stale socket file
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = match tokio::net::UnixListener::bind(&self.socket_path) {
            Ok(l) => {
                if let Err(error) = std::fs::set_permissions(
                    &self.socket_path,
                    std::fs::Permissions::from_mode(0o600),
                ) {
                    warn!(%error, path = ?self.socket_path, "unix: failed to secure socket");
                    let _ = std::fs::remove_file(&self.socket_path);
                    return;
                }
                info!(path = ?self.socket_path, "unix channel listening");
                l
            }
            Err(e) => {
                warn!(error = %e, "unix: bind failed");
                return;
            }
        };

        ctx.mark_ready();

        let hub = Arc::new(Mutex::new(RelayHub::default()));
        let next_id: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
        let mut client_tasks = tokio::task::JoinSet::new();

        loop {
            let accepted = tokio::select! {
                result = listener.accept() => result,
                () = ctx.shutdown_requested() => break,
            };
            let (stream, _addr) = match accepted {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "unix: accept failed");
                    continue;
                }
            };

            let peer = match stream.peer_cred() {
                Ok(peer) => peer,
                Err(error) => {
                    warn!(%error, "unix: could not authenticate peer credentials");
                    if let Some(ui) = &ctx.ui {
                        let boundary = sylvander_protocol::BoundaryContext::unauthenticated(
                            &self.instance_id,
                            "unix",
                            uuid::Uuid::new_v4().to_string(),
                        );
                        let _ = ui
                            .reject_authentication(
                                &boundary,
                                sylvander_protocol::AuthenticationFailure::new(
                                    sylvander_protocol::AuthenticationMethod::UnixPeer,
                                ),
                            )
                            .await;
                    }
                    continue;
                }
            };
            let principal = sylvander_protocol::AuthenticatedPrincipal::user(
                format!("unix:{}:uid:{}", self.instance_id, peer.uid()),
                sylvander_protocol::AuthenticationMethod::UnixPeer,
            );
            info!(uid = peer.uid(), "unix: authenticated local socket owner");

            let (read, mut write) = stream.into_split();
            let mut reader = FramedRead::new(
                read,
                LinesCodec::new_with_max_length(self.max_request_bytes),
            );

            let client_id = {
                let mut id = next_id.lock().await;
                *id += 1;
                *id
            };

            let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
            hub.lock().await.clients.insert(client_id, tx.clone());

            // Spawn writer task
            let writer_hub = hub.clone();
            client_tasks.spawn(async move {
                while let Some(msg) = rx.recv().await {
                    if let Ok(s) = serde_json::to_string(&msg)
                        && (write.write_all(s.as_bytes()).await.is_err()
                            || write.write_all(b"\n").await.is_err())
                    {
                        break;
                    }
                }
                detach_client(&writer_hub, client_id).await;
            });

            // Spawn reader task
            let ctx_clone = ctx.clone();
            let agent_id_clone = agent_id.clone();
            let runtime = self.runtime.clone();
            let reader_hub = hub.clone();
            let client_id_clone = client_id;
            let instance_id = self.instance_id.clone();
            client_tasks.spawn(async move {
                let mut negotiated_version = None;
                while let Some(result) = reader.next().await {
                    let line = match result {
                        Ok(line) => line,
                        Err(error) => {
                            warn!(%error, "unix: rejected oversized or invalid frame");
                            let _ = tx.send(ServerMsg::ProtocolError {
                                error: protocol_error(
                                    "frame_too_large",
                                    "request exceeds the configured frame limit",
                                ),
                            });
                            break;
                        }
                    };
                    let msg: ClientMsg = match serde_json::from_str(&line) {
                        Ok(m) => m,
                        Err(e) => {
                            warn!(error = %e, bytes = line.len(), "unix: bad client message");
                            let _ = tx.send(ServerMsg::ProtocolError {
                                error: protocol_error("malformed_message", &e.to_string()),
                            });
                            if negotiated_version.is_none() {
                                break;
                            }
                            continue;
                        }
                    };
                    if negotiated_version.is_none() {
                        let ClientMsg::Hello { protocol } = msg else {
                            let _ = tx.send(ServerMsg::ProtocolError {
                                error: protocol_error(
                                    "handshake_required",
                                    "hello must be the first client message",
                                ),
                            });
                            break;
                        };
                        match sylvander_protocol::negotiate_ui_protocol(&protocol) {
                            Ok(version) => {
                                negotiated_version = Some(version);
                                let _ = tx.send(ServerMsg::Welcome {
                                    protocol: sylvander_protocol::UiProtocolWelcome {
                                        server_name: "sylvander-server".into(),
                                        version,
                                        capabilities: ui_protocol_capabilities(version),
                                    },
                                });
                            }
                            Err(error) => {
                                let _ = tx.send(ServerMsg::ProtocolError { error });
                                break;
                            }
                        }
                        continue;
                    }
                    if matches!(msg, ClientMsg::Hello { .. }) {
                        let _ = tx.send(ServerMsg::ProtocolError {
                            error: protocol_error(
                                "duplicate_handshake",
                                "connection is already negotiated",
                            ),
                        });
                        continue;
                    }
                    let boundary = sylvander_protocol::BoundaryContext::authenticated(
                        principal.clone(),
                        &instance_id,
                        "unix",
                        uuid::Uuid::new_v4().to_string(),
                    );
                    handle_client_msg_for_client(
                        msg,
                        ClientHandler {
                            boundary: &boundary,
                            ctx: &ctx_clone,
                            agent_id: &agent_id_clone,
                            tx: &tx,
                            runtime: &runtime,
                            hub: &reader_hub,
                            client_id,
                            ui_protocol_version: negotiated_version
                                .expect("successful handshake records a version"),
                        },
                    )
                    .await;
                }
                detach_client(&reader_hub, client_id_clone).await;
            });
        }
        client_tasks.abort_all();
        while client_tasks.join_next().await.is_some() {}
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

fn ui_protocol_capabilities(version: u16) -> Vec<String> {
    [
        ("agent_administration", 2),
        ("attachments", 1),
        ("approval_scopes", 1),
        ("compaction", 1),
        ("credential_registry_lifecycle", 3),
        ("diagnostics", 1),
        ("model_selection", 1),
        ("plans", 1),
        ("provider_model_registry_lifecycle", 3),
        ("registry_administration", 2),
        ("session_replay", 1),
        ("sessions", 1),
        ("tasks", 1),
        (sylvander_protocol::USER_PROFILE_CAPABILITY, 1),
        ("workspace_rollback", 1),
    ]
    .into_iter()
    .filter(|(_, minimum)| version >= *minimum)
    .map(|(capability, _)| capability.to_owned())
    .collect()
}

fn protocol_error(code: &str, message: &str) -> sylvander_protocol::UiProtocolError {
    sylvander_protocol::UiProtocolError {
        code: code.into(),
        message: message.chars().take(240).collect(),
        server_min_version: sylvander_protocol::UI_PROTOCOL_MIN_VERSION,
        server_max_version: sylvander_protocol::UI_PROTOCOL_MAX_VERSION,
    }
}

async fn detach_client(hub: &Arc<Mutex<RelayHub>>, client_id: u64) {
    let mut hub = hub.lock().await;
    hub.clients.remove(&client_id);
    for clients in hub.session_clients.values_mut() {
        clients.remove(&client_id);
    }
}

async fn relay_event(hub: &Arc<Mutex<RelayHub>>, session_id: &SessionId, message: ServerMsg) {
    const MAX_REPLAY_BYTES: usize = 4 * 1024 * 1024;
    let mut hub = hub.lock().await;
    let terminal = matches!(
        &message,
        ServerMsg::Done { .. } | ServerMsg::Error { .. } | ServerMsg::TurnInterrupted { .. }
    );
    let replay = hub.replay.entry(session_id.clone()).or_default();
    if replay.active {
        let bytes = serde_json::to_vec(&message).map_or(0, |value| value.len());
        if bytes > MAX_REPLAY_BYTES {
            replay.events.clear();
            replay.bytes = 0;
            replay.truncated = true;
        } else {
            replay.bytes = replay.bytes.saturating_add(bytes);
            replay.events.push(message.clone());
            while replay.bytes > MAX_REPLAY_BYTES && replay.events.len() > 1 {
                let removed = replay.events.remove(0);
                replay.bytes = replay
                    .bytes
                    .saturating_sub(serde_json::to_vec(&removed).map_or(0, |value| value.len()));
                replay.truncated = true;
            }
        }
        if terminal {
            replay.active = false;
            replay.events.clear();
            replay.bytes = 0;
            replay.truncated = false;
        }
    }
    if terminal {
        hub.relays.remove(session_id);
    }
    let recipients = hub
        .session_clients
        .get(session_id)
        .into_iter()
        .flatten()
        .filter_map(|id| hub.clients.get(id).cloned())
        .collect::<Vec<_>>();
    drop(hub);
    for recipient in recipients {
        let _ = recipient.send(message.clone());
    }
}

#[cfg(test)]
async fn handle_client_msg(
    msg: ClientMsg,
    ctx: &ChannelContext,
    agent_id: &AgentId,
    tx: &mpsc::UnboundedSender<ServerMsg>,
    runtime: &RuntimeInfo,
) {
    let hub = Arc::new(Mutex::new(RelayHub::default()));
    hub.lock().await.clients.insert(0, tx.clone());
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        sylvander_protocol::AuthenticatedPrincipal::user(
            "unix-client",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        ),
        "unix",
        "unix",
        "test-request",
    );
    handle_client_msg_for_client(
        msg,
        ClientHandler {
            boundary: &boundary,
            ctx,
            agent_id,
            tx,
            runtime,
            hub: &hub,
            client_id: 0,
            ui_protocol_version: sylvander_protocol::UI_PROTOCOL_MAX_VERSION,
        },
    )
    .await;
}

struct ClientHandler<'a> {
    boundary: &'a sylvander_protocol::BoundaryContext,
    ctx: &'a ChannelContext,
    agent_id: &'a AgentId,
    tx: &'a mpsc::UnboundedSender<ServerMsg>,
    runtime: &'a RuntimeInfo,
    hub: &'a Arc<Mutex<RelayHub>>,
    client_id: u64,
    ui_protocol_version: u16,
}

async fn handle_client_msg_for_client(msg: ClientMsg, handler: ClientHandler<'_>) {
    let ClientHandler {
        boundary,
        ctx,
        agent_id,
        tx,
        runtime,
        hub,
        client_id,
        ui_protocol_version,
    } = handler;
    if let ClientMsg::RegistryAdmin { request } = &msg
        && request.minimum_ui_protocol_version() > ui_protocol_version
    {
        let _ = tx.send(ServerMsg::ProtocolError {
            error: protocol_error(
                "unsupported_message_version",
                "message requires a newer UI protocol version",
            ),
        });
        return;
    }
    if !matches!(&msg, ClientMsg::Chat { .. })
        && let Some(ui) = &ctx.ui
        && let Err(error) = ui.authorize_message(boundary, &msg).await
    {
        boundary_denied(tx, error);
        return;
    }
    let principal_id = boundary
        .principal
        .as_ref()
        .map_or("__unauthenticated__", |principal| principal.id.0.as_str());
    match msg {
        ClientMsg::Hello { .. } => {}
        ClientMsg::Chat {
            text,
            attachments,
            session_id,
            workspace,
        } => {
            let existing_session = session_id.map(SessionId::new);
            let workspace = workspace.map(PathBuf::from);
            let session_name = workspace
                .as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("Sylvander session")
                .to_string();
            let overrides = SessionConfigOverrides {
                user_workspace: workspace.map(|path| SessionWorkspaceBinding {
                    execution_target: "local".into(),
                    path,
                    read_only: false,
                }),
                ..SessionConfigOverrides::default()
            };
            if let Some(session_id) = &existing_session
                && hub
                    .lock()
                    .await
                    .replay
                    .get(session_id)
                    .is_some_and(|replay| replay.active)
            {
                operation_error(tx, "chat", "session already has an active turn");
                return;
            }
            let submitted = match submit_external_chat(
                ctx,
                boundary,
                ExternalChatRequest {
                    existing_session,
                    agent_id: agent_id.clone(),
                    label: session_name,
                    overrides,
                    text: text.clone(),
                    attachments: attachments.clone(),
                    external_meta: std::collections::BTreeMap::new(),
                },
            )
            .await
            {
                Ok(submitted) => submitted,
                Err(error) => {
                    boundary_denied(tx, error);
                    return;
                }
            };
            let sid = submitted.session_id;

            let start_relay = {
                let mut guard = hub.lock().await;
                if guard.replay.get(&sid).is_some_and(|replay| replay.active) {
                    drop(guard);
                    operation_error(tx, "chat", "session already has an active turn");
                    return;
                }
                guard
                    .session_clients
                    .entry(sid.clone())
                    .or_default()
                    .insert(client_id);
                let replay = guard.replay.entry(sid.clone()).or_default();
                replay.active = true;
                replay.events.clear();
                replay.bytes = 0;
                replay.truncated = false;
                guard.relays.insert(sid.clone())
            };
            let relay_rx = if start_relay {
                Some(submitted.events)
            } else {
                drop(submitted.events);
                None
            };

            // Notify client of session
            let _ = tx.send(ServerMsg::SessionCreated {
                session_id: sid.0.clone(),
                config: None,
            });

            // Stream events back to client until Done
            let relay_hub = hub.clone();
            if let Some(mut rx) = relay_rx {
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
                                    cause,
                                } => Some(ServerMsg::ModelRetry {
                                    session_id: s.0.clone(),
                                    attempt,
                                    max_attempts,
                                    delay_ms,
                                    reason,
                                    cause,
                                }),
                                StreamEvent::InteractionTimedOut {
                                    kind,
                                    subject_id,
                                    timeout_secs,
                                    recovery,
                                } => Some(ServerMsg::InteractionTimeout {
                                    session_id: s.0.clone(),
                                    kind,
                                    subject_id,
                                    timeout_secs,
                                    recovery,
                                }),
                                StreamEvent::CompactionStarted { automatic } => {
                                    Some(ServerMsg::CompactionStarted {
                                        session_id: s.0.clone(),
                                        automatic,
                                    })
                                }
                                StreamEvent::CompactionCompleted { report } => {
                                    Some(ServerMsg::CompactionCompleted {
                                        session_id: s.0.clone(),
                                        report,
                                    })
                                }
                                StreamEvent::CompactionFailed { automatic, reason } => {
                                    Some(ServerMsg::CompactionFailed {
                                        session_id: s.0.clone(),
                                        automatic,
                                        reason,
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
                                    cost_nano_usd,
                                } => Some(ServerMsg::IterationEnd {
                                    session_id: s.0.clone(),
                                    iteration,
                                    input_tokens,
                                    output_tokens,
                                    cost_nano_usd,
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
                                StreamEvent::Done { text } => Some(ServerMsg::Done {
                                    session_id: s.0.clone(),
                                    text,
                                }),
                                StreamEvent::UserAnswer { .. } => None,
                            };
                            if let Some(m) = out {
                                let terminal = matches!(
                                    &m,
                                    ServerMsg::Done { .. }
                                        | ServerMsg::Error { .. }
                                        | ServerMsg::TurnInterrupted { .. }
                                );
                                relay_event(&relay_hub, s, m).await;
                                if terminal {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        }
        ClientMsg::Approve {
            session_id,
            call_id,
            approved,
            scope,
            reason,
        } => {
            if let Err(error) = ctx
                .submit_control(
                    boundary,
                    ClientMsg::Approve {
                        session_id,
                        call_id,
                        approved,
                        scope,
                        reason,
                    },
                )
                .await
            {
                boundary_denied(tx, error);
            }
        }
        ClientMsg::Answer {
            session_id,
            call_id,
            answer,
        } => {
            if let Err(error) = ctx
                .submit_control(
                    boundary,
                    ClientMsg::Answer {
                        session_id,
                        call_id,
                        answer,
                    },
                )
                .await
            {
                boundary_denied(tx, error);
            }
        }
        ClientMsg::Interrupt { session_id } => {
            if let Err(error) = ctx
                .submit_control(boundary, ClientMsg::Interrupt { session_id })
                .await
            {
                boundary_denied(tx, error);
            }
        }
        ClientMsg::ResolvePlan {
            session_id,
            plan_id,
            decision,
        } => {
            if let Err(error) = ctx
                .submit_control(
                    boundary,
                    ClientMsg::ResolvePlan {
                        session_id,
                        plan_id,
                        decision,
                    },
                )
                .await
            {
                boundary_denied(tx, error);
            }
        }
        ClientMsg::CancelTask {
            session_id,
            task_id,
        } => {
            if let Err(error) = ctx
                .submit_control(
                    boundary,
                    ClientMsg::CancelTask {
                        session_id,
                        task_id,
                    },
                )
                .await
            {
                boundary_denied(tx, error);
            }
        }
        ClientMsg::DiscoverAgents => {
            if let Some(ui) = &ctx.ui {
                match ui.discover_agents(boundary).await {
                    Ok(agents) => {
                        let _ = tx.send(ServerMsg::AgentsDiscovered { agents });
                    }
                    Err(error) => boundary_denied(tx, error),
                }
            } else {
                operation_error(tx, "discover_agents", "UI service is unavailable");
            }
        }
        ClientMsg::CreateSession { request } => {
            if let Some(ui) = &ctx.ui {
                match ui.create_session(boundary, request).await {
                    Ok(config) => {
                        let _ = tx.send(ServerMsg::SessionCreated {
                            session_id: config.session_id.0.clone(),
                            config: Some(config),
                        });
                    }
                    Err(error) => boundary_denied(tx, error),
                }
            } else {
                operation_error(tx, "create_session", "UI service is unavailable");
            }
        }
        ClientMsg::GetSessionConfig { session_id } => {
            if let Some(ui) = &ctx.ui {
                match ui
                    .session_config(boundary, &SessionId::new(session_id))
                    .await
                {
                    Ok(state) => {
                        let _ = tx.send(ServerMsg::SessionConfig { state });
                    }
                    Err(error) => boundary_denied(tx, error),
                }
            } else {
                operation_error(tx, "get_session_config", "UI service is unavailable");
            }
        }
        ClientMsg::UpdateSessionConfig { request } => {
            if let Some(ui) = &ctx.ui {
                match ui.update_session_config(boundary, request).await {
                    Ok(state) => {
                        let _ = tx.send(ServerMsg::SessionConfig { state });
                    }
                    Err(error) => boundary_denied(tx, error),
                }
            } else {
                operation_error(tx, "update_session_config", "UI service is unavailable");
            }
        }
        ClientMsg::SubmitFeedback { feedback } => {
            if let Some(ui) = &ctx.ui {
                match ui.submit_feedback(boundary, feedback).await {
                    Ok(feedback_id) => {
                        let _ = tx.send(ServerMsg::FeedbackRecorded { feedback_id });
                    }
                    Err(error) => boundary_denied(tx, error),
                }
            } else {
                operation_error(tx, "submit_feedback", "UI service is unavailable");
            }
        }
        ClientMsg::AgentAdmin { request } => {
            let response = if let Some(ui) = &ctx.ui {
                ui.agent_admin(boundary, request).await
            } else {
                unavailable_agent_admin_response()
            };
            let _ = tx.send(ServerMsg::AgentAdmin { response });
        }
        ClientMsg::RegistryAdmin { request } => {
            let response = if let Some(ui) = &ctx.ui {
                ui.registry_admin(boundary, request).await
            } else {
                unavailable_registry_admin_response()
            };
            let _ = tx.send(ServerMsg::RegistryAdmin { response });
        }
        ClientMsg::UserProfile { request } => {
            let response = if let Some(ui) = &ctx.ui {
                ui.user_profile(boundary, request).await
            } else {
                sylvander_protocol::UserProfileResponse::Error {
                    version: sylvander_protocol::USER_PROFILE_PROTOCOL_VERSION,
                    error: sylvander_protocol::UserProfileError::service_unavailable(
                        request.operation(),
                    ),
                }
            };
            let _ = tx.send(ServerMsg::UserProfile { response });
        }
        ClientMsg::ListSessions => {
            let caller = sylvander_protocol::SessionContext::new(
                principal_id,
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
        request @ (ClientMsg::LoadSession { .. } | ClientMsg::ReattachSession { .. }) => {
            let recovery = matches!(&request, ClientMsg::ReattachSession { .. });
            let (ClientMsg::LoadSession { session_id } | ClientMsg::ReattachSession { session_id }) =
                request
            else {
                unreachable!()
            };
            let session_id = SessionId::new(session_id);
            let caller = unix_session_context(principal_id, agent_id, session_id.clone());
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
                        let mut history = ServerMsg::SessionHistory {
                            session: session_info(session),
                            messages,
                            iterations: usage.iterations,
                            input_tokens: usage.input_tokens,
                            output_tokens: usage.output_tokens,
                            cost_nano_usd: usage.cost_nano_usd,
                            notice: None,
                            source_session_id: None,
                            recovery,
                            replay_truncated: false,
                        };
                        let mut hub = hub.lock().await;
                        for clients in hub.session_clients.values_mut() {
                            clients.remove(&client_id);
                        }
                        hub.session_clients
                            .entry(session_id.clone())
                            .or_default()
                            .insert(client_id);
                        let replay = hub.replay.get(&session_id);
                        let truncated = replay.is_some_and(|replay| replay.truncated);
                        let events = replay
                            .map(|replay| replay.events.clone())
                            .unwrap_or_default();
                        let ServerMsg::SessionHistory {
                            replay_truncated, ..
                        } = &mut history
                        else {
                            unreachable!()
                        };
                        *replay_truncated = truncated;
                        let _ = tx.send(history);
                        for event in events {
                            let _ = tx.send(event);
                        }
                        drop(hub);
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
            match ctx
                .sessions
                .patch_metadata(
                    &session_id,
                    SessionMetadataPatch {
                        name: Some(label.clone()),
                        external_meta: std::collections::HashMap::new(),
                    },
                )
                .await
            {
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
        ClientMsg::ForkSession {
            session_id,
            completed_turns,
            checkpoint,
        } => {
            if checkpoint && completed_turns.is_some() {
                operation_error(
                    tx,
                    "fork_session",
                    "checkpoint and completed_turns are mutually exclusive",
                );
                return;
            }
            let source_id = SessionId::new(session_id);
            let caller = unix_session_context(principal_id, agent_id, source_id.clone());
            match ctx.sessions.get(&source_id).await {
                Ok(Some(source)) => {
                    let fork_id = SessionId::new(uuid::Uuid::new_v4().to_string());
                    let mut fork = source.clone();
                    fork.id = fork_id.clone();
                    fork.name = completed_turns.map_or_else(
                        || {
                            if checkpoint {
                                format!("{} (checkpoint)", source.name)
                            } else {
                                format!("{} (fork)", source.name)
                            }
                        },
                        |turn| format!("{} (rewind {turn})", source.name),
                    );
                    fork.metadata.name = fork.name.clone();
                    fork.created_at = sylvander_agent::session::now_secs();
                    fork.updated_at = fork.created_at;
                    let mut history = match ctx
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
                    if let Some(turns) = completed_turns {
                        let boundary = history
                            .iter()
                            .enumerate()
                            .filter(|(_, message)| {
                                message.role
                                    == sylvander_agent::session_store::MessageRole::Assistant
                            })
                            .nth(turns.saturating_sub(1))
                            .map(|(index, _)| index + 1);
                        let Some(boundary) = boundary else {
                            operation_error(
                                tx,
                                "rewind_session",
                                format!("completed turn {turns} does not exist"),
                            );
                            return;
                        };
                        history.truncate(boundary);
                    }
                    if let Err(error) = ctx.sessions.save(&fork).await {
                        warn!(%error, "unix: failed to save forked session");
                        return;
                    }
                    let fork_caller = unix_session_context(principal_id, agent_id, fork_id.clone());
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
                            let _ = ctx.sessions.delete(&fork_id).await;
                            operation_error(tx, "fork_session", error.to_string());
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
                        cost_nano_usd: Some(0),
                        notice: completed_turns.map(|turn| {
                            format!(
                                "Conversation rewound through completed turn {turn} · source session and workspace files unchanged"
                            )
                        }).or_else(|| checkpoint.then(|| {
                            "Conversation checkpoint branch created · source session and workspace files unchanged".into()
                        })),
                        source_session_id: Some(source_id.0.clone()),
                        recovery: false,
                        replay_truncated: false,
                    });
                }
                Ok(None) => warn!(%source_id, "unix: fork source not found"),
                Err(error) => warn!(%error, "unix: failed to get fork source"),
            }
        }
        ClientMsg::GetRuntimeInfo => {
            let capabilities = runtime
                .models
                .iter()
                .find(|model| model.id == runtime.model)
                .map_or(runtime.capabilities, |model| model.capabilities);
            let model_selection = unique_model_selection(&runtime.models, &runtime.model);
            let _ = tx.send(ServerMsg::RuntimeInfo {
                model: runtime.model.clone(),
                model_selection,
                reasoning_effort: runtime.reasoning_effort,
                models: runtime.models.clone(),
                permissions: runtime.permissions.clone(),
                capabilities,
                approval_enabled: runtime.approval_enabled,
                max_attachment_bytes: runtime.max_attachment_bytes,
                platform: runtime.platform.clone(),
            });
        }
        ClientMsg::GetContext { session_id } => {
            let (Some(ui), Some(session_id)) = (&ctx.ui, session_id.as_deref()) else {
                let _ = tx.send(ServerMsg::OperationError {
                    operation: "context".into(),
                    message: "context requires an authenticated session".into(),
                });
                return;
            };
            match ui
                .context_report(boundary, &SessionId::new(session_id))
                .await
            {
                Ok(report) => {
                    let _ = tx.send(ServerMsg::ContextReport { report });
                }
                Err(error) => boundary_denied(tx, error),
            }
        }
        ClientMsg::Compact { session_id } => {
            let Some(ui) = &ctx.ui else {
                let _ = tx.send(ServerMsg::OperationError {
                    operation: "compact".into(),
                    message: "UI service is unavailable".into(),
                });
                return;
            };
            let _ = tx.send(ServerMsg::CompactionStarted {
                session_id: session_id.clone(),
                automatic: false,
            });
            match ui
                .compact_session(boundary, &SessionId::new(session_id.clone()))
                .await
            {
                Ok(report) => {
                    let _ = tx.send(ServerMsg::CompactionCompleted { session_id, report });
                }
                Err(reason) => {
                    let _ = tx.send(ServerMsg::CompactionFailed {
                        session_id,
                        automatic: false,
                        reason: reason.message,
                    });
                }
            }
        }
        ClientMsg::PreviewWorkspaceRollback { session_id } => {
            let Some(ui) = &ctx.ui else {
                let _ = tx.send(ServerMsg::WorkspaceRollbackFailed {
                    session_id,
                    reason: "UI service is unavailable".into(),
                });
                return;
            };
            match ui
                .preview_workspace_rollback(boundary, &SessionId::new(session_id.clone()))
                .await
            {
                Ok(preview) => {
                    let _ = tx.send(ServerMsg::WorkspaceRollbackPreview {
                        session_id,
                        preview,
                    });
                }
                Err(reason) => {
                    let _ = tx.send(ServerMsg::WorkspaceRollbackFailed {
                        session_id,
                        reason: reason.message,
                    });
                }
            }
        }
        ClientMsg::RollbackWorkspace {
            session_id,
            expected_turn_id,
        } => {
            let Some(ui) = &ctx.ui else {
                let _ = tx.send(ServerMsg::WorkspaceRollbackFailed {
                    session_id,
                    reason: "UI service is unavailable".into(),
                });
                return;
            };
            match ui
                .rollback_workspace(
                    boundary,
                    &SessionId::new(session_id.clone()),
                    &expected_turn_id,
                )
                .await
            {
                Ok(report) => {
                    let _ = tx.send(ServerMsg::WorkspaceRollbackCompleted { session_id, report });
                }
                Err(reason) => {
                    let _ = tx.send(ServerMsg::WorkspaceRollbackFailed {
                        session_id,
                        reason: reason.message,
                    });
                }
            }
        }
        ClientMsg::InspectCodingSession { session_id } => {
            let Some(ui) = &ctx.ui else {
                let _ = tx.send(ServerMsg::CodingSessionOperationFailed {
                    session_id,
                    operation: "inspect".into(),
                    reason: "UI service is unavailable".into(),
                });
                return;
            };
            match ui
                .inspect_coding_session(boundary, &SessionId::new(session_id.clone()))
                .await
            {
                Ok(diff) => {
                    let _ = tx.send(ServerMsg::CodingSessionDiff { session_id, diff });
                }
                Err(error) => {
                    let _ = tx.send(ServerMsg::CodingSessionOperationFailed {
                        session_id,
                        operation: "inspect".into(),
                        reason: error.message,
                    });
                }
            }
        }
        ClientMsg::AcceptCodingSession { session_id } => {
            let Some(ui) = &ctx.ui else {
                let _ = tx.send(ServerMsg::CodingSessionOperationFailed {
                    session_id,
                    operation: "accept".into(),
                    reason: "UI service is unavailable".into(),
                });
                return;
            };
            match ui
                .accept_coding_session(boundary, &SessionId::new(session_id.clone()))
                .await
            {
                Ok(()) => {
                    let _ = tx.send(ServerMsg::CodingSessionAccepted { session_id });
                }
                Err(error) => {
                    let _ = tx.send(ServerMsg::CodingSessionOperationFailed {
                        session_id,
                        operation: "accept".into(),
                        reason: error.message,
                    });
                }
            }
        }
        ClientMsg::DiscardCodingSession { session_id } => {
            let Some(ui) = &ctx.ui else {
                let _ = tx.send(ServerMsg::CodingSessionOperationFailed {
                    session_id,
                    operation: "discard".into(),
                    reason: "UI service is unavailable".into(),
                });
                return;
            };
            match ui
                .discard_coding_session(boundary, &SessionId::new(session_id.clone()))
                .await
            {
                Ok(()) => {
                    let _ = tx.send(ServerMsg::CodingSessionDiscarded { session_id });
                }
                Err(error) => {
                    let _ = tx.send(ServerMsg::CodingSessionOperationFailed {
                        session_id,
                        operation: "discard".into(),
                        reason: error.message,
                    });
                }
            }
        }
        ClientMsg::SelectModel {
            session_id,
            model,
            reasoning_effort,
        } => {
            let Some(session_id) = session_id else {
                let _ = tx.send(ServerMsg::OperationError {
                    operation: "select_model".into(),
                    message: "select_model requires a session_id".into(),
                });
                return;
            };
            let Some(ui) = &ctx.ui else {
                operation_error(tx, "select_model", "UI service is unavailable");
                return;
            };
            let session_id = SessionId::new(session_id);
            let state = match ui.session_config(boundary, &session_id).await {
                Ok(state) => state,
                Err(error) => {
                    boundary_denied(tx, error);
                    return;
                }
            };
            let mut overrides = state.overrides;
            let agents = match ui.discover_agents(boundary).await {
                Ok(agents) => agents,
                Err(error) => {
                    boundary_denied(tx, error);
                    return;
                }
            };
            let catalog = visible_model_catalog(&agents, &state.effective.agent_id);
            let selection = match model.resolve(&catalog) {
                Ok(selection) => selection,
                Err(error) => {
                    operation_error(tx, "select_model", error.to_string());
                    return;
                }
            };
            overrides.model = Some(selection);
            overrides.model_id = None;
            overrides.reasoning_effort = Some(reasoning_effort);
            match ui
                .update_session_config(
                    boundary,
                    sylvander_protocol::SessionConfigUpdateRequest {
                        session_id,
                        expected_revision: state.revision,
                        overrides,
                    },
                )
                .await
            {
                Ok(state) => send_session_runtime_info(tx, runtime, &state.effective),
                Err(error) => boundary_denied(tx, error),
            }
        }
        ClientMsg::SelectPermissions {
            session_id,
            profile,
        } => {
            let Some(session_id) = session_id else {
                let _ = tx.send(ServerMsg::OperationError {
                    operation: "select_permissions".into(),
                    message: "select_permissions requires a session_id".into(),
                });
                return;
            };
            let Some(ui) = &ctx.ui else {
                operation_error(tx, "select_permissions", "UI service is unavailable");
                return;
            };
            let session_id = SessionId::new(session_id);
            let state = match ui.session_config(boundary, &session_id).await {
                Ok(state) => state,
                Err(error) => {
                    boundary_denied(tx, error);
                    return;
                }
            };
            let mut overrides = state.overrides;
            overrides.permissions = Some(profile);
            match ui
                .update_session_config(
                    boundary,
                    sylvander_protocol::SessionConfigUpdateRequest {
                        session_id,
                        expected_revision: state.revision,
                        overrides,
                    },
                )
                .await
            {
                Ok(state) => send_session_runtime_info(tx, runtime, &state.effective),
                Err(error) => boundary_denied(tx, error),
            }
        }
        ClientMsg::Ping => {
            let _ = tx.send(ServerMsg::Pong);
        }
    }
}

fn unique_model_selection(
    models: &[sylvander_protocol::ModelDescriptor],
    model_id: &str,
) -> Option<sylvander_protocol::ModelSelection> {
    let mut matches = models.iter().filter(|model| model.id == model_id);
    let model = matches.next()?;
    matches
        .next()
        .is_none()
        .then(|| sylvander_protocol::ModelSelection {
            provider_id: model.provider.clone(),
            model_id: model.id.clone(),
        })
}

fn send_session_runtime_info(
    tx: &mpsc::UnboundedSender<ServerMsg>,
    runtime: &RuntimeInfo,
    effective: &sylvander_protocol::SessionEffectiveConfig,
) {
    let capabilities = runtime
        .models
        .iter()
        .find(|entry| entry.id == effective.model_id && entry.provider == effective.provider_id)
        .map_or(0, |entry| entry.capabilities);
    let _ = tx.send(ServerMsg::RuntimeInfo {
        model: effective.model_id.clone(),
        model_selection: Some(effective.model_selection()),
        reasoning_effort: effective.reasoning_effort,
        models: runtime.models.clone(),
        permissions: effective.permissions.clone(),
        capabilities,
        approval_enabled: runtime.approval_enabled,
        max_attachment_bytes: runtime.max_attachment_bytes,
        platform: runtime.platform.clone(),
    });
}

fn visible_model_catalog(
    agents: &[sylvander_protocol::AgentDescriptor],
    agent_id: &AgentId,
) -> Vec<sylvander_protocol::ModelSelection> {
    let Some(agent) = agents.iter().find(|agent| agent.id == *agent_id) else {
        return Vec::new();
    };
    agent
        .models
        .iter()
        .map(|model| sylvander_protocol::ModelSelection {
            provider_id: model.provider.clone(),
            model_id: model.id.clone(),
        })
        .collect()
}

fn unix_session_context(
    principal_id: &str,
    agent_id: &AgentId,
    session_id: SessionId,
) -> sylvander_protocol::SessionContext {
    sylvander_protocol::SessionContext::new(principal_id, agent_id.clone(), session_id)
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

fn boundary_denied(
    tx: &mpsc::UnboundedSender<ServerMsg>,
    error: sylvander_protocol::BoundaryError,
) {
    let _ = tx.send(ServerMsg::BoundaryDenied { error });
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
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use sylvander_agent::bus::{
        BusMessage, InProcessMessageBus, MessageBus, SubscriptionFilter, SystemMessage,
    };
    use sylvander_agent::session_store::{
        MessageRole, SessionLifetime, SessionStore, SqliteSessionStore, StoredSession,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[derive(Default)]
    struct EmptyUiService {
        registry_authorizations: AtomicUsize,
        registry_dispatches: AtomicUsize,
        allow_registry: bool,
        session_config: Option<sylvander_protocol::SessionConfigState>,
        chat_bus: Option<Arc<dyn MessageBus>>,
        compaction: Option<sylvander_protocol::CompactionReport>,
        rollback_preview: Option<sylvander_protocol::WorkspaceRollbackPreview>,
        rollback_report: Option<sylvander_protocol::WorkspaceRollbackReport>,
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_deserialization() {
        let (mut client, server) = tokio::io::duplex(64);
        let mut reader = FramedRead::new(server, LinesCodec::new_with_max_length(4));
        client.write_all(b"12345\n").await.unwrap();

        assert!(reader.next().await.unwrap().is_err());
    }

    #[async_trait]
    impl sylvander_channel::UiService for EmptyUiService {
        async fn authorize_message(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            message: &ClientMsg,
        ) -> Result<(), sylvander_protocol::BoundaryError> {
            if matches!(message, ClientMsg::RegistryAdmin { .. }) {
                self.registry_authorizations.fetch_add(1, Ordering::Relaxed);
            }
            if matches!(message, ClientMsg::RegistryAdmin { .. })
                && !self.allow_registry
                && !boundary
                    .principal
                    .as_ref()
                    .is_some_and(|principal| principal.has_role("admin"))
            {
                return Err(sylvander_protocol::BoundaryError::forbidden(
                    boundary,
                    "registry_admin",
                ));
            }
            Ok(())
        }

        async fn submit_chat(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            request: sylvander_channel::ExternalChatRequest,
        ) -> Result<sylvander_channel::SubmittedChat, sylvander_protocol::BoundaryError> {
            let bus = self.chat_bus.as_ref().ok_or_else(|| {
                sylvander_protocol::BoundaryError::forbidden(boundary, "submit_chat")
            })?;
            let principal = boundary.principal.as_ref().ok_or_else(|| {
                sylvander_protocol::BoundaryError::unauthenticated(boundary, "submit_chat")
            })?;
            let session_id = request
                .existing_session
                .unwrap_or_else(|| SessionId::new(uuid::Uuid::new_v4().to_string()));
            let events = bus
                .subscribe(SubscriptionFilter {
                    session_ids: Some(vec![session_id.clone()]),
                    recipients: None,
                    kinds: None,
                })
                .await
                .map_err(|_| {
                    sylvander_protocol::BoundaryError::forbidden(boundary, "submit_chat")
                })?;
            bus.publish(BusMessage {
                session_id: session_id.clone(),
                sender: sylvander_agent::bus::Sender::User(principal.id.0.clone()),
                recipient: sylvander_agent::bus::Recipient::Agent(request.agent_id),
                kind: MessageKind::Chat,
                payload: request.text,
                attachments: request.attachments,
                timestamp: sylvander_agent::session::now_secs(),
                id: sylvander_agent::bus::MessageId::new(),
            })
            .await
            .map_err(|_| sylvander_protocol::BoundaryError::forbidden(boundary, "submit_chat"))?;
            Ok(sylvander_channel::SubmittedChat { session_id, events })
        }

        async fn submit_control(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            message: ClientMsg,
        ) -> Result<(), sylvander_protocol::BoundaryError> {
            let bus = self.chat_bus.as_ref().ok_or_else(|| {
                sylvander_protocol::BoundaryError::forbidden(boundary, "submit_control")
            })?;
            let (session_id, system) = match message {
                ClientMsg::Approve {
                    session_id,
                    call_id,
                    approved,
                    scope,
                    reason,
                } => (
                    SessionId::new(session_id),
                    SystemMessage::ApproveTool {
                        call_id,
                        approved,
                        scope,
                        reason,
                    },
                ),
                ClientMsg::ResolvePlan {
                    session_id,
                    plan_id,
                    decision,
                } => (
                    SessionId::new(session_id),
                    SystemMessage::ResolvePlan { plan_id, decision },
                ),
                ClientMsg::CancelTask {
                    session_id,
                    task_id,
                } => {
                    let session_id = SessionId::new(session_id);
                    (
                        session_id.clone(),
                        SystemMessage::CancelTask {
                            session_id,
                            task_id,
                        },
                    )
                }
                _ => {
                    return Err(sylvander_protocol::BoundaryError::forbidden(
                        boundary,
                        "submit_control",
                    ));
                }
            };
            bus.publish(BusMessage {
                session_id,
                sender: sylvander_agent::bus::Sender::System,
                recipient: sylvander_agent::bus::Recipient::Agent(AgentId::new("agent-1")),
                kind: MessageKind::System(system),
                payload: String::new(),
                attachments: Vec::new(),
                timestamp: sylvander_agent::session::now_secs(),
                id: sylvander_agent::bus::MessageId::new(),
            })
            .await
            .map_err(|_| sylvander_protocol::BoundaryError::forbidden(boundary, "submit_control"))
        }

        async fn discover_agents(
            &self,
            _boundary: &sylvander_protocol::BoundaryContext,
        ) -> Result<Vec<sylvander_protocol::AgentDescriptor>, sylvander_protocol::BoundaryError>
        {
            Ok(Vec::new())
        }

        async fn create_session(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            _request: sylvander_protocol::SessionCreateRequest,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
            Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                "create_session",
            ))
        }

        async fn session_config(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            _session_id: &SessionId,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
            self.session_config.clone().ok_or_else(|| {
                sylvander_protocol::BoundaryError::forbidden(boundary, "get_session_config")
            })
        }

        async fn update_session_config(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            _request: sylvander_protocol::SessionConfigUpdateRequest,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
            Err(sylvander_protocol::BoundaryError::forbidden(
                boundary,
                "update_session_config",
            ))
        }

        async fn submit_feedback(
            &self,
            _boundary: &sylvander_protocol::BoundaryContext,
            _feedback: sylvander_protocol::RunFeedback,
        ) -> Result<String, sylvander_protocol::BoundaryError> {
            Ok("feedback-1".into())
        }

        async fn compact_session(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            _session_id: &SessionId,
        ) -> Result<sylvander_protocol::CompactionReport, sylvander_protocol::BoundaryError>
        {
            self.compaction.clone().ok_or_else(|| {
                sylvander_protocol::BoundaryError::forbidden(boundary, "compact_session")
            })
        }

        async fn preview_workspace_rollback(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            _session_id: &SessionId,
        ) -> Result<sylvander_protocol::WorkspaceRollbackPreview, sylvander_protocol::BoundaryError>
        {
            self.rollback_preview.clone().ok_or_else(|| {
                sylvander_protocol::BoundaryError::forbidden(boundary, "preview_workspace_rollback")
            })
        }

        async fn rollback_workspace(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            _session_id: &SessionId,
            _expected_turn_id: &str,
        ) -> Result<sylvander_protocol::WorkspaceRollbackReport, sylvander_protocol::BoundaryError>
        {
            self.rollback_report.clone().ok_or_else(|| {
                sylvander_protocol::BoundaryError::forbidden(boundary, "rollback_workspace")
            })
        }

        async fn registry_admin(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            request: sylvander_protocol::RegistryAdminRequest,
        ) -> sylvander_protocol::RegistryAdminResponse {
            self.registry_dispatches.fetch_add(1, Ordering::Relaxed);
            assert!(
                self.allow_registry
                    || boundary
                        .principal
                        .as_ref()
                        .is_some_and(|principal| principal.has_role("admin")),
                "non-administrator reached registry dispatch"
            );
            let result = match request {
                sylvander_protocol::RegistryAdminRequest::InspectProviderRevision {
                    provider_id,
                    revision,
                } => sylvander_protocol::RegistryAdminResult::ProviderRevisionInspected {
                    revision: sylvander_protocol::ProviderRevisionView {
                        definition: sylvander_protocol::RedactedProviderDefinition {
                            provider_id,
                            revision,
                            kind: "mock".into(),
                            base_url_sha256: "base-digest".into(),
                            credential_binding_id_sha256: "binding-digest".into(),
                        },
                        digest_sha256: "definition-digest".into(),
                        created_at_unix_secs: 7,
                        active: true,
                    },
                },
                sylvander_protocol::RegistryAdminRequest::CreateCredentialBinding { .. } => {
                    sylvander_protocol::RegistryAdminResult::CredentialBindingCreated {
                        generation: sylvander_protocol::CredentialGenerationView {
                            binding_id_sha256: "binding-id-digest".into(),
                            generation: 1,
                            reference_kind:
                                sylvander_protocol::CredentialReferenceKind::Environment,
                            reference_configured: true,
                            reference_digest_sha256: "reference-digest".into(),
                            created_at_unix_secs: 7,
                            active: true,
                        },
                    }
                }
                _ => unreachable!(),
            };
            sylvander_protocol::RegistryAdminResponse::Success {
                result: Box::new(result),
            }
        }
    }

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
                capability_names: Vec::new(),
                reasoning_efforts: vec![sylvander_protocol::ReasoningEffort::Off],
                lifecycle: sylvander_protocol::ModelLifecycle::Active,
                pricing: None,
            }],
            permissions: sylvander_protocol::PermissionProfile::default(),
            capabilities: 0b101,
            approval_enabled: true,
            max_attachment_bytes: 1024,
            platform: sylvander_protocol::PlatformSnapshot::default(),
        }
    }

    fn private_session_config(
        session_id: &str,
        prompt: &str,
        digest: &str,
    ) -> sylvander_protocol::SessionConfigState {
        use sylvander_protocol::{
            PromptLayerDigest, PromptLayerKind, PromptManifest, SessionConfigProvenance,
            SessionConfigSource, SessionConfigSourceKind, SessionEffectiveConfig,
        };
        let source = SessionConfigSource {
            kind: SessionConfigSourceKind::SessionOverride,
            reference: Some("session".into()),
        };
        sylvander_protocol::SessionConfigState {
            session_id: SessionId::new(session_id),
            revision: 2,
            overrides: sylvander_protocol::SessionConfigOverrides {
                system_prompt: Some(prompt.into()),
                ..sylvander_protocol::SessionConfigOverrides::default()
            },
            effective: SessionEffectiveConfig {
                agent_id: AgentId::new("agent-1"),
                agent_revision: 1,
                provider_id: "test".into(),
                provider_revision: Some(1),
                model_id: "test-model".into(),
                model_revision: Some(1),
                reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
                permissions: sylvander_protocol::PermissionProfile::default(),
                prompt_profile: None,
                system_prompt_sha256: digest.into(),
                prompt_manifest: Some(PromptManifest {
                    layers: vec![PromptLayerDigest {
                        kind: PromptLayerKind::SessionInput,
                        reference: Some("session".into()),
                        sha256: digest.into(),
                        byte_count: prompt.len() as u64,
                    }],
                    aggregate_sha256: "aggregate-digest".into(),
                    total_bytes: prompt.len() as u64,
                }),
                agent_workspace: None,
                user_workspace: None,
                execution_target: "local".into(),
                provenance: SessionConfigProvenance {
                    model: source.clone(),
                    reasoning_effort: source.clone(),
                    permissions: source.clone(),
                    prompt_profile: source.clone(),
                    system_prompt: source.clone(),
                    agent_workspace: source.clone(),
                    user_workspace: source.clone(),
                    execution_target: source,
                },
            },
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
        let line = send_and_read_wire(write, reader, message).await;
        serde_json::from_str(&line).expect("json response")
    }

    async fn send_and_read_wire(
        write: &mut tokio::net::unix::OwnedWriteHalf,
        reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
        message: serde_json::Value,
    ) -> String {
        write
            .write_all(format!("{message}\n").as_bytes())
            .await
            .expect("write");
        tokio::time::timeout(std::time::Duration::from_secs(1), reader.next_line())
            .await
            .expect("response timeout")
            .expect("read")
            .expect("response")
    }

    async fn negotiate(
        write: &mut tokio::net::unix::OwnedWriteHalf,
        reader: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    ) {
        let welcome = send_and_read(
            write,
            reader,
            serde_json::json!({
                "type":"hello",
                "protocol": {
                    "client_name":"channel-test",
                    "min_version":1,
                    "max_version":1,
                    "capabilities":[]
                }
            }),
        )
        .await;
        assert_eq!(welcome["type"], "welcome");
        assert_eq!(welcome["protocol"]["version"], 1);
    }

    #[tokio::test]
    async fn runtime_info_reports_server_truth() {
        let bus = Arc::new(InProcessMessageBus::new());
        let context = ChannelContext::with_services(
            bus,
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            None,
            None,
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::GetRuntimeInfo,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;

        let response = rx.recv().await.expect("runtime response");
        assert!(matches!(
            response,
            ServerMsg::RuntimeInfo {
                model,
                model_selection: Some(selection),
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
                ..
            } if model == "test-model"
                && selection.provider_id == "test"
                && selection.model_id == "test-model"
                && models.len() == 1
        ));
    }

    #[tokio::test]
    async fn agent_discovery_is_served_through_the_ui_service_boundary() {
        let context = ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            Some(Arc::new(EmptyUiService::default())),
            None,
        );
        let (tx, mut rx) = mpsc::unbounded_channel();

        handle_client_msg(
            ClientMsg::DiscoverAgents,
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;

        assert!(matches!(
            rx.recv().await.expect("discovery response"),
            ServerMsg::AgentsDiscovered { agents } if agents.is_empty()
        ));

        handle_client_msg(
            ClientMsg::SubmitFeedback {
                feedback: sylvander_protocol::RunFeedback {
                    run_id: "run-1".into(),
                    turn_id: None,
                    rating: sylvander_protocol::FeedbackRating::Positive,
                    note: None,
                    tags: Vec::new(),
                },
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;
        assert!(matches!(
            rx.recv().await.expect("feedback response"),
            ServerMsg::FeedbackRecorded { feedback_id } if feedback_id == "feedback-1"
        ));
    }

    #[tokio::test]
    async fn agent_admin_without_ui_service_returns_content_free_error() {
        let context = ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            None,
            None,
        );
        let (tx, mut rx) = mpsc::unbounded_channel();

        handle_client_msg(
            ClientMsg::AgentAdmin {
                request: sylvander_protocol::AgentAdminRequest::InspectRevision {
                    agent_id: AgentId::new("private-agent"),
                    revision: 42,
                },
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;

        let response = rx.recv().await.expect("Agent admin response");
        let json = serde_json::to_string(&response).expect("serialize response");
        assert!(matches!(
            response,
            ServerMsg::AgentAdmin {
                response: sylvander_protocol::AgentAdminResponse::Error {
                    error: sylvander_protocol::AgentAdminError {
                        code: sylvander_protocol::AgentAdminErrorCode::Unauthorized,
                        agent_id: None,
                        revision: None,
                        ..
                    }
                }
            }
        ));
        assert!(!json.contains("private-agent"));
        assert!(!json.contains("42"));
    }

    #[tokio::test]
    async fn registry_admin_without_ui_service_returns_content_free_error() {
        let context = ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            None,
            None,
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::RegistryAdmin {
                request: sylvander_protocol::RegistryAdminRequest::InspectProviderRevision {
                    provider_id: "private-provider".into(),
                    revision: 42,
                },
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;

        let response = rx.recv().await.expect("registry admin response");
        let json = serde_json::to_string(&response).unwrap();
        assert!(matches!(
            response,
            ServerMsg::RegistryAdmin {
                response: sylvander_protocol::RegistryAdminResponse::Error {
                    error: sylvander_protocol::RegistryAdminError {
                        code: sylvander_protocol::RegistryAdminErrorCode::Unauthorized,
                        provider_id: None,
                        revision: None,
                        ..
                    }
                }
            }
        ));
        assert!(!json.contains("private-provider"));
        assert!(!json.contains("42"));
    }

    fn inspect_registry_request() -> ClientMsg {
        serde_json::from_value(serde_json::json!({
            "type": "registry_admin",
            "request": {
                "operation": "inspect_provider_revision",
                "provider_id": "provider-a",
                "revision": 9
            }
        }))
        .expect("decode registry request")
    }

    async fn dispatch_client_message_as(
        principal: sylvander_protocol::AuthenticatedPrincipal,
        request: ClientMsg,
    ) -> ServerMsg {
        let context = ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            Some(Arc::new(EmptyUiService::default())),
            None,
        );
        let boundary = sylvander_protocol::BoundaryContext::authenticated(
            principal,
            "unix-test",
            "unix",
            "request-1",
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_client_msg_for_client(
            request,
            ClientHandler {
                boundary: &boundary,
                ctx: &context,
                agent_id: &AgentId::new("agent-1"),
                tx: &tx,
                runtime: &runtime_info(),
                hub: &Arc::new(Mutex::new(RelayHub::default())),
                client_id: 1,
                ui_protocol_version: sylvander_protocol::UI_PROTOCOL_MAX_VERSION,
            },
        )
        .await;
        rx.recv().await.expect("registry transport response")
    }

    #[tokio::test]
    async fn registry_admin_round_trip_preserves_success_response() {
        let mut principal = sylvander_protocol::AuthenticatedPrincipal::user(
            "admin",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        );
        principal.roles.push("admin".into());
        let response = dispatch_client_message_as(principal, inspect_registry_request()).await;
        let wire = serde_json::to_string(&response).expect("encode registry response");
        let decoded: ServerMsg = serde_json::from_str(&wire).expect("decode registry response");

        assert!(matches!(
            decoded,
            ServerMsg::RegistryAdmin {
                response: sylvander_protocol::RegistryAdminResponse::Success { result }
            } if matches!(
                result.as_ref(),
                sylvander_protocol::RegistryAdminResult::ProviderRevisionInspected { revision }
                    if revision.definition.provider_id == "provider-a"
                        && revision.definition.revision == 9
            )
        ));
    }

    #[tokio::test]
    async fn registry_admin_non_administrator_is_rejected_before_dispatch() {
        let principal = sylvander_protocol::AuthenticatedPrincipal::user(
            "reader",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        );
        assert!(matches!(
            dispatch_client_message_as(principal, inspect_registry_request()).await,
            ServerMsg::BoundaryDenied { error }
                if error.code == sylvander_protocol::BoundaryErrorCode::Forbidden
                    && error.operation == "registry_admin"
        ));
    }

    #[test]
    fn server_advertises_administration_capabilities() {
        let v1 = ui_protocol_capabilities(1);
        let v2 = ui_protocol_capabilities(2);
        let v3 = ui_protocol_capabilities(3);
        assert!(!v1.iter().any(|item| item.contains("administration")));
        assert!(
            !v1.iter()
                .any(|item| item == "credential_registry_lifecycle")
        );
        assert!(v2.iter().any(|item| item == "agent_administration"));
        assert!(v2.iter().any(|item| item == "registry_administration"));
        for capabilities in [&v1, &v2] {
            assert!(
                !capabilities
                    .iter()
                    .any(|item| item == "provider_model_registry_lifecycle")
            );
        }
        assert!(
            !v2.iter()
                .any(|item| item == "credential_registry_lifecycle")
        );
        assert!(
            v3.iter()
                .any(|item| item == "credential_registry_lifecycle")
        );
        assert!(
            v3.iter()
                .any(|item| item == "provider_model_registry_lifecycle")
        );
    }

    #[tokio::test]
    async fn negotiated_version_gates_registry_mutations_before_dispatch() {
        let path = socket_path();
        let service = Arc::new(EmptyUiService {
            allow_registry: true,
            ..EmptyUiService::default()
        });
        let channel = Arc::new(UnixChannel::new(&path, "agent-1"));
        let task = tokio::spawn(channel.run(ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            Some(service.clone()),
            None,
        )));
        let mutation = serde_json::json!({
            "type": "registry_admin",
            "request": {
                "operation": "create_credential_binding",
                "binding_id": "credential/private-binding",
                "reference": {"source": "environment", "name": "PRIVATE_API_KEY"}
            }
        });

        let stream = connect(&path).await;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let first = send_and_read(&mut write, &mut lines, mutation.clone()).await;
        assert_eq!(first["error"]["code"], "handshake_required");
        assert_eq!(service.registry_authorizations.load(Ordering::Relaxed), 0);
        assert_eq!(service.registry_dispatches.load(Ordering::Relaxed), 0);

        let stream = connect(&path).await;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let hello_v2 = serde_json::json!({
            "type": "hello",
            "protocol": {
                "client_name": "v2-client", "min_version": 2, "max_version": 2,
                "capabilities": []
            }
        });
        let welcome = send_and_read(&mut write, &mut lines, hello_v2.clone()).await;
        assert_eq!(welcome["protocol"]["version"], 2);
        assert!(
            welcome["protocol"]["capabilities"]
                .as_array()
                .is_some_and(|values| values
                    .iter()
                    .any(|value| value == "registry_administration"))
        );
        assert!(
            !welcome["protocol"]["capabilities"]
                .as_array()
                .is_some_and(|values| values
                    .iter()
                    .any(|value| value == "credential_registry_lifecycle"))
        );
        let duplicate = send_and_read(&mut write, &mut lines, hello_v2).await;
        assert_eq!(duplicate["error"]["code"], "duplicate_handshake");
        let rejected = send_and_read(&mut write, &mut lines, mutation.clone()).await;
        assert_eq!(rejected["error"]["code"], "unsupported_message_version");
        let rejected_wire = rejected.to_string();
        assert!(!rejected_wire.contains("credential/private-binding"));
        assert!(!rejected_wire.contains("PRIVATE_API_KEY"));
        assert_eq!(service.registry_authorizations.load(Ordering::Relaxed), 0);
        assert_eq!(service.registry_dispatches.load(Ordering::Relaxed), 0);

        let stream = connect(&path).await;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let welcome = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({
                "type": "hello",
                "protocol": {
                    "client_name": "v3-client", "min_version": 3, "max_version": 3,
                    "capabilities": []
                }
            }),
        )
        .await;
        assert_eq!(welcome["protocol"]["version"], 3);
        let accepted = send_and_read(&mut write, &mut lines, mutation).await;
        assert_eq!(accepted["type"], "registry_admin");
        assert_eq!(accepted["response"]["status"], "success");
        assert_eq!(service.registry_authorizations.load(Ordering::Relaxed), 1);
        assert_eq!(service.registry_dispatches.load(Ordering::Relaxed), 1);

        task.abort();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn session_prompt_is_redacted_on_the_unix_wire() {
        const SENTINEL: &str = "UNIX_PRIVATE_SESSION_PROMPT_SENTINEL";
        const DIGEST: &str = "unix-public-prompt-digest";
        let path = socket_path();
        let service = Arc::new(EmptyUiService {
            session_config: Some(private_session_config("session-secret", SENTINEL, DIGEST)),
            ..EmptyUiService::default()
        });
        let channel = Arc::new(UnixChannel::new(&path, "agent-1"));
        let task = tokio::spawn(channel.run(ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            Some(service),
            None,
        )));
        let stream = connect(&path).await;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        negotiate(&mut write, &mut lines).await;

        let wire = send_and_read_wire(
            &mut write,
            &mut lines,
            serde_json::json!({
                "type": "get_session_config",
                "session_id": "session-secret"
            }),
        )
        .await;
        let response: serde_json::Value = serde_json::from_str(&wire).expect("session config");

        assert!(!wire.contains(SENTINEL));
        assert!(
            response["state"]["overrides"]
                .get("system_prompt")
                .is_none()
        );
        assert_eq!(
            response["state"]["effective"]["system_prompt_sha256"],
            DIGEST
        );
        assert_eq!(
            response["state"]["effective"]["prompt_manifest"]["layers"][0]["sha256"],
            DIGEST
        );

        task.abort();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn credential_create_round_trip_returns_only_redacted_view() {
        let binding_id = "credential/private-binding";
        let locator = "PRIVATE_PROVIDER_API_KEY";
        let request: ClientMsg = serde_json::from_value(serde_json::json!({
            "type": "registry_admin",
            "request": {
                "operation": "create_credential_binding",
                "binding_id": binding_id,
                "reference": {
                    "source": "environment",
                    "name": locator
                }
            }
        }))
        .expect("decode credential create request");
        let mut principal = sylvander_protocol::AuthenticatedPrincipal::user(
            "admin",
            sylvander_protocol::AuthenticationMethod::UnixPeer,
        );
        principal.roles.push("admin".into());

        let response = dispatch_client_message_as(principal, request).await;
        let wire = serde_json::to_string(&response).expect("encode credential response");
        assert!(!wire.contains(binding_id));
        assert!(!wire.contains(locator));
        assert!(matches!(
            response,
            ServerMsg::RegistryAdmin {
                response: sylvander_protocol::RegistryAdminResponse::Success { result }
            } if matches!(
                result.as_ref(),
                sylvander_protocol::RegistryAdminResult::CredentialBindingCreated { generation }
                    if generation.generation == 1
                        && generation.reference_configured
                        && generation.binding_id_sha256 == "binding-id-digest"
            )
        ));
    }

    #[tokio::test]
    async fn model_selection_without_session_fails_closed() {
        let bus = Arc::new(InProcessMessageBus::new());
        let context = ChannelContext::with_services(
            bus,
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            Some(Arc::new(EmptyUiService::default())),
            None,
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::SelectModel {
                session_id: None,
                model: sylvander_protocol::ModelSelectionInput::Legacy("thinking-model".into()),
                reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;

        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::OperationError { operation, message })
                if operation == "select_model" && message.contains("session_id")
        ));

        handle_client_msg(
            ClientMsg::SelectPermissions {
                session_id: None,
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
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::OperationError { operation, message })
                if operation == "select_permissions" && message.contains("session_id")
        ));

        handle_client_msg(
            ClientMsg::Compact {
                session_id: "missing-session".into(),
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::CompactionStarted {
                automatic: false,
                ..
            })
        ));
        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::CompactionFailed {
                automatic: false,
                reason,
                ..
            }) if reason == "the principal is not allowed to access this resource"
        ));
    }

    #[tokio::test]
    async fn workspace_rollback_preview_and_confirmation_round_trip() {
        let bus = Arc::new(InProcessMessageBus::new());
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let ui = EmptyUiService {
            rollback_preview: Some(sylvander_protocol::WorkspaceRollbackPreview {
                turn_id: "turn-1".into(),
                files: vec!["file.txt".into()],
            }),
            rollback_report: Some(sylvander_protocol::WorkspaceRollbackReport {
                turn_id: "turn-1".into(),
                restored: vec!["file.txt".into()],
            }),
            ..EmptyUiService::default()
        };
        let context = ChannelContext::with_services(
            bus,
            Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
            Some(Arc::new(ui)),
            None,
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::PreviewWorkspaceRollback {
                session_id: session_id.0.clone(),
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;
        let turn_id = match rx.recv().await.unwrap() {
            ServerMsg::WorkspaceRollbackPreview { preview, .. } => preview.turn_id,
            other => panic!("unexpected preview response: {other:?}"),
        };
        handle_client_msg(
            ClientMsg::RollbackWorkspace {
                session_id: session_id.0.clone(),
                expected_turn_id: turn_id,
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &runtime_info(),
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::WorkspaceRollbackCompleted { .. })
        ));
    }

    #[tokio::test]
    async fn persisted_session_load_rename_fork_and_archive_round_trip() {
        let path = socket_path();
        let agent_id = AgentId::new("agent-1");
        let store: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store"));
        let session_id = SessionId::new("session-1");
        let credential_probe = tempfile::NamedTempFile::new().expect("credential probe");
        let principal_id = format!(
            "unix:unix:uid:{}",
            credential_probe
                .as_file()
                .metadata()
                .expect("credential metadata")
                .uid()
        );
        let metadata = sylvander_agent::session::SessionMetadata {
            workspace: "/workspace/project".into(),
            name: "Original".into(),
            user_id: principal_id.clone(),
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
        let caller = unix_session_context(&principal_id, &agent_id, session_id.clone());
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
        for (role, content) in [
            (MessageRole::Assistant, "answer one"),
            (MessageRole::User, "question two"),
            (MessageRole::Assistant, "answer two"),
        ] {
            store
                .append_message(
                    &caller,
                    &session_id,
                    role,
                    serde_json::json!({"role": match role { MessageRole::User => "user", _ => "assistant" }, "content": content}),
                    None,
                    None,
                    None,
                )
                .await
                .expect("append turn");
        }
        store
            .record_usage(&session_id, 120, 30, Some(45_000))
            .await
            .expect("usage");

        let channel = Arc::new(UnixChannel::new(&path, agent_id));
        let context = ChannelContext::with_services(
            Arc::new(InProcessMessageBus::new()),
            store.clone(),
            Some(Arc::new(EmptyUiService::default())),
            None,
        );
        let task = tokio::spawn(channel.run(context));
        let stream = connect(&path).await;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        negotiate(&mut write, &mut lines).await;

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
        assert_eq!(forked["source_session_id"], "session-1");

        let checkpoint = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({
                "type":"fork_session",
                "session_id":"session-1",
                "checkpoint":true
            }),
        )
        .await;
        assert!(
            checkpoint["session"]["label"]
                .as_str()
                .unwrap()
                .contains("checkpoint")
        );
        assert!(
            checkpoint["notice"]
                .as_str()
                .unwrap()
                .contains("workspace files unchanged")
        );

        let rewound = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({
                "type":"fork_session",
                "session_id":"session-1",
                "completed_turns":1
            }),
        )
        .await;
        assert_eq!(rewound["type"], "session_history");
        assert_eq!(rewound["messages"].as_array().unwrap().len(), 2);
        assert!(
            rewound["session"]["label"]
                .as_str()
                .unwrap()
                .contains("rewind 1")
        );
        assert!(
            rewound["notice"]
                .as_str()
                .unwrap()
                .contains("workspace files unchanged")
        );
        let invalid_rewind = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({
                "type":"fork_session",
                "session_id":"session-1",
                "completed_turns":99
            }),
        )
        .await;
        assert_eq!(invalid_rewind["type"], "operation_error");
        assert_eq!(invalid_rewind["operation"], "rewind_session");

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
    async fn reconnect_replays_the_complete_in_flight_turn() {
        let path = socket_path();
        let agent_id = AgentId::new("agent-1");
        let bus = Arc::new(InProcessMessageBus::new());
        let store: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store"));
        store
            .save(&StoredSession::new(
                SessionId::new("session-1"),
                "Recovery",
                SessionLifetime::Persistent,
                sylvander_agent::session::SessionMetadata {
                    workspace: "/workspace/project".into(),
                    name: "Recovery".into(),
                    user_id: "unix-client".into(),
                },
                vec![agent_id.clone()],
            ))
            .await
            .expect("save");
        let channel = Arc::new(UnixChannel::new(&path, agent_id.clone()));
        let ui = EmptyUiService {
            chat_bus: Some(bus.clone()),
            ..EmptyUiService::default()
        };
        let task = tokio::spawn(channel.run(ChannelContext::with_services(
            bus.clone(),
            store,
            Some(Arc::new(ui)),
            None,
        )));

        let stream = connect(&path).await;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        negotiate(&mut write, &mut lines).await;
        let created = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({
                "type":"chat",
                "text":"continue",
                "session_id":"session-1"
            }),
        )
        .await;
        assert_eq!(created["type"], "session_created");
        bus.publish(BusMessage::stream_event(
            SessionId::new("session-1"),
            agent_id.clone(),
            StreamEvent::TextDelta {
                delta: "before ".into(),
            },
        ))
        .await
        .expect("first delta");
        assert!(lines.next_line().await.unwrap().unwrap().contains("before"));
        let concurrent = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({"type":"chat","text":"race","session_id":"session-1"}),
        )
        .await;
        assert_eq!(concurrent["type"], "operation_error");
        assert_eq!(concurrent["operation"], "chat");
        drop(lines);
        drop(write);

        bus.publish(BusMessage::stream_event(
            SessionId::new("session-1"),
            agent_id,
            StreamEvent::TextDelta {
                delta: "after".into(),
            },
        ))
        .await
        .expect("missed delta");

        let stream = connect(&path).await;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        negotiate(&mut write, &mut lines).await;
        let history = send_and_read(
            &mut write,
            &mut lines,
            serde_json::json!({"type":"reattach_session","session_id":"session-1"}),
        )
        .await;
        assert_eq!(history["type"], "session_history");
        assert_eq!(history["recovery"], true);
        let replayed = [
            lines.next_line().await.unwrap().unwrap(),
            lines.next_line().await.unwrap().unwrap(),
        ]
        .join(" ");
        assert!(replayed.contains("before"));
        assert!(replayed.contains("after"));

        task.abort();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn socket_permissions_and_live_events_are_isolated_between_clients() {
        let path = socket_path();
        let agent_id = AgentId::new("agent-1");
        let bus = Arc::new(InProcessMessageBus::new());
        let store: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store"));
        let channel = Arc::new(UnixChannel::new(&path, agent_id.clone()));
        let ui = EmptyUiService {
            chat_bus: Some(bus.clone()),
            ..EmptyUiService::default()
        };
        let task = tokio::spawn(channel.run(ChannelContext::with_services(
            bus.clone(),
            store,
            Some(Arc::new(ui)),
            None,
        )));

        let stream_a = connect(&path).await;
        assert_eq!(
            std::fs::metadata(&path)
                .expect("socket metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the local Agent socket must not be accessible to other OS users"
        );
        let (read_a, mut write_a) = stream_a.into_split();
        let mut lines_a = BufReader::new(read_a).lines();
        negotiate(&mut write_a, &mut lines_a).await;

        let stream_b = connect(&path).await;
        let (read_b, mut write_b) = stream_b.into_split();
        let mut lines_b = BufReader::new(read_b).lines();
        negotiate(&mut write_b, &mut lines_b).await;

        let created_a = send_and_read(
            &mut write_a,
            &mut lines_a,
            serde_json::json!({"type":"chat","text":"a","session_id":"session-a"}),
        )
        .await;
        let created_b = send_and_read(
            &mut write_b,
            &mut lines_b,
            serde_json::json!({"type":"chat","text":"b","session_id":"session-b"}),
        )
        .await;
        assert_eq!(created_a["session_id"], "session-a");
        assert_eq!(created_b["session_id"], "session-b");

        for (session, delta) in [("session-a", "only-a"), ("session-b", "only-b")] {
            bus.publish(BusMessage::stream_event(
                SessionId::new(session),
                agent_id.clone(),
                StreamEvent::TextDelta {
                    delta: delta.into(),
                },
            ))
            .await
            .expect("publish isolated event");
        }

        let event_a: serde_json::Value = serde_json::from_str(
            &tokio::time::timeout(std::time::Duration::from_secs(1), lines_a.next_line())
                .await
                .expect("client A timeout")
                .expect("client A read")
                .expect("client A event"),
        )
        .expect("client A json");
        let event_b: serde_json::Value = serde_json::from_str(
            &tokio::time::timeout(std::time::Duration::from_secs(1), lines_b.next_line())
                .await
                .expect("client B timeout")
                .expect("client B read")
                .expect("client B event"),
        )
        .expect("client B json");
        assert_eq!(event_a["session_id"], "session-a");
        assert_eq!(event_a["delta"], "only-a");
        assert_eq!(event_b["session_id"], "session-b");
        assert_eq!(event_b["delta"], "only-b");

        bus.publish(BusMessage::stream_event(
            SessionId::new("session-a"),
            agent_id,
            StreamEvent::TextDelta {
                delta: "still-only-a".into(),
            },
        ))
        .await
        .expect("publish follow-up");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), lines_b.next_line())
                .await
                .is_err(),
            "client B received an event from client A's session"
        );

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
        let ui = EmptyUiService {
            chat_bus: Some(bus.clone()),
            ..EmptyUiService::default()
        };
        let context = ChannelContext::with_services(
            bus,
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            Some(Arc::new(ui)),
            None,
        );
        let (tx, _rx) = mpsc::unbounded_channel();

        handle_client_msg(
            ClientMsg::ResolvePlan {
                session_id: "session-1".into(),
                plan_id: "plan-1".into(),
                decision: sylvander_protocol::PlanDecision::Revised {
                    steps: vec!["inspect".into(), "verify".into()],
                },
            },
            &context,
            &agent_id,
            &tx,
            &runtime_info(),
        )
        .await;

        let message = inbox.recv().await.expect("agent message");
        assert!(matches!(
            (message.session_id.0.as_str(), message.kind),
            ("session-1",
            MessageKind::System(SystemMessage::ResolvePlan {
                plan_id,
                decision: sylvander_protocol::PlanDecision::Revised { steps },
            })) if plan_id == "plan-1" && steps == ["inspect", "verify"]
        ));
    }

    #[tokio::test]
    async fn approval_decision_is_forwarded_without_transport_interpretation() {
        let bus = Arc::new(InProcessMessageBus::new());
        let agent_id = AgentId::new("agent-1");
        let mut inbox = bus
            .subscribe(SubscriptionFilter::for_agent(agent_id.clone()))
            .await
            .expect("subscribe");
        let ui = EmptyUiService {
            chat_bus: Some(bus.clone()),
            ..EmptyUiService::default()
        };
        let context = ChannelContext::with_services(
            bus,
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            Some(Arc::new(ui)),
            None,
        );
        let (tx, _rx) = mpsc::unbounded_channel();
        handle_client_msg(
            ClientMsg::Approve {
                session_id: "session-1".into(),
                call_id: "call-1".into(),
                approved: false,
                scope: sylvander_protocol::ApprovalScope::Session,
                reason: Some("unsafe outside workspace".into()),
            },
            &context,
            &agent_id,
            &tx,
            &runtime_info(),
        )
        .await;

        let message = inbox.recv().await.expect("agent message");
        assert!(matches!(
            (message.session_id.0.as_str(), message.kind),
            ("session-1",
            MessageKind::System(SystemMessage::ApproveTool {
                call_id,
                approved: false,
                scope: sylvander_protocol::ApprovalScope::Session,
                reason: Some(reason),
            })) if call_id == "call-1" && reason == "unsafe outside workspace"
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
        let ui = EmptyUiService {
            chat_bus: Some(bus.clone()),
            ..EmptyUiService::default()
        };
        let context = ChannelContext::with_services(
            bus,
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            Some(Arc::new(ui)),
            None,
        );
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
        let ui = EmptyUiService {
            chat_bus: Some(bus.clone()),
            ..EmptyUiService::default()
        };
        let context = ChannelContext::with_services(
            bus,
            Arc::new(SqliteSessionStore::open_in_memory().await.expect("store")),
            Some(Arc::new(ui)),
            None,
        );
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
