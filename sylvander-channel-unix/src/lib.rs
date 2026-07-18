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
    pub platform_provider:
        Option<Arc<dyn Fn() -> sylvander_protocol::PlatformSnapshot + Send + Sync>>,
}

impl RuntimeInfo {
    fn platform_snapshot(&self) -> sylvander_protocol::PlatformSnapshot {
        self.platform_provider
            .as_ref()
            .map_or_else(|| self.platform.clone(), |provider| provider())
    }
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
                platform_provider: None,
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
        (sylvander_protocol::IDENTITY_BINDING_CAPABILITY, 1),
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
                    instruction_focus: None,
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
        ClientMsg::IdentityBinding { request } => {
            let operation = request.operation();
            let response = match Arc::into_inner(request) {
                Some(request) => ctx.submit_identity_binding(boundary, request).await,
                None => sylvander_protocol::IdentityBindingResponse::Error {
                    version: sylvander_protocol::IDENTITY_BINDING_PROTOCOL_VERSION,
                    error: sylvander_protocol::IdentityBindingError::service_unavailable(operation),
                },
            };
            let _ = tx.send(ServerMsg::IdentityBinding {
                response: Arc::new(response),
            });
        }
        ClientMsg::ListSessions => {
            if let Some(ui) = &ctx.ui {
                match ui.list_sessions(boundary).await {
                    Ok(sessions) => {
                        let _ = tx.send(ServerMsg::SessionsList { sessions });
                    }
                    Err(error) => {
                        warn!(error = %error, "unix: failed to list sessions");
                        operation_error(tx, "list_sessions", error.to_string());
                    }
                }
                return;
            }
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
                platform: runtime.platform_snapshot(),
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
        platform: runtime.platform_snapshot(),
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
#[path = "../tests/unit/lib.rs"]
mod tests;
