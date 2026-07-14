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

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

use sylvander_agent::bus::{
    BusMessage, MessageKind, StreamEvent, SubscriptionFilter, SystemMessage,
};
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{Channel, ChannelContext, ExternalChatRequest, authorize_external_chat};
use sylvander_protocol::{
    UiClientMessage as ClientMsg, UiServerMessage as ServerMsg, UiToolInfo as ToolInfo,
};

// ===========================================================================
// Channel
// ===========================================================================

pub struct WsChannel {
    addr: SocketAddr,
    agent_id: AgentId,
    instance_id: String,
    auth: Option<WsAuth>,
    max_request_bytes: usize,
}

#[derive(Clone)]
struct WsAuth {
    principal_id: String,
    bearer_token: String,
}

impl WsChannel {
    pub fn new(addr: SocketAddr, agent_id: impl Into<AgentId>) -> Self {
        Self {
            addr,
            agent_id: agent_id.into(),
            instance_id: "websocket".into(),
            auth: None,
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
        self.auth = Some(WsAuth {
            principal_id: principal_id.into(),
            bearer_token: bearer_token.into(),
        });
        self
    }
}

#[async_trait]
impl Channel for WsChannel {
    fn name(&self) -> &'static str {
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
            instance_id: self.instance_id.clone(),
            auth: self.auth.clone(),
            max_request_bytes: self.max_request_bytes,
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
    instance_id: String,
    auth: Option<WsAuth>,
    max_request_bytes: usize,
}

// ===========================================================================
// WebSocket upgrade
// ===========================================================================

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(principal) = authenticate(&state, &headers) else {
        warn!(instance = %state.instance_id, "ws: rejected unauthenticated upgrade");
        return reject_ws_authentication(&state).await.into_response();
    };
    ws.max_frame_size(state.max_request_bytes)
        .max_message_size(state.max_request_bytes)
        .on_upgrade(move |socket| handle_socket(socket, state, principal))
        .into_response()
}

async fn reject_ws_authentication(state: &AppState) -> StatusCode {
    let boundary = sylvander_protocol::BoundaryContext::unauthenticated(
        &state.instance_id,
        "websocket",
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
        boundary_status(&error)
    } else {
        StatusCode::UNAUTHORIZED
    }
}

fn boundary_status(error: &sylvander_protocol::BoundaryError) -> StatusCode {
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
    let auth = state.auth.as_ref()?;
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    constant_time_eq(supplied.as_bytes(), auth.bearer_token.as_bytes()).then(|| {
        sylvander_protocol::AuthenticatedPrincipal::user(
            auth.principal_id.clone(),
            sylvander_protocol::AuthenticationMethod::BearerToken,
        )
    })
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

async fn handle_socket(
    socket: WebSocket,
    state: Arc<AppState>,
    principal: sylvander_protocol::AuthenticatedPrincipal,
) {
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
            if let Ok(s) = serde_json::to_string(&msg)
                && ws_tx.send(Message::Text(s.into())).await.is_err()
            {
                break;
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
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => continue,
        };

        let parsed: ClientMsg = match serde_json::from_str(&msg) {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, msg = %msg, "ws: bad json");
                continue;
            }
        };
        handle_client_msg(parsed, &ctx, &agent_id, &tx, &principal, &state.instance_id).await;
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
    principal: &sylvander_protocol::AuthenticatedPrincipal,
    instance_id: &str,
) {
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        principal.clone(),
        instance_id,
        "websocket",
        uuid::Uuid::new_v4().to_string(),
    );
    if !matches!(msg, ClientMsg::Hello { .. } | ClientMsg::Chat { .. })
        && let Some(ui) = &ctx.ui
        && let Err(error) = ui.authorize_message(&boundary, &msg).await
    {
        boundary_denied(tx, error);
        return;
    }
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
            workspace: _,
        } => {
            let existing_session = session_id.map(SessionId::new);
            let sid = match authorize_external_chat(
                ctx,
                &boundary,
                ExternalChatRequest {
                    existing_session,
                    agent_id: agent_id.clone(),
                    label: "websocket session".into(),
                    overrides: sylvander_protocol::SessionConfigOverrides::default(),
                    text: text.clone(),
                    attachments: attachments.clone(),
                    external_meta: BTreeMap::from([(
                        "channel_instance_id".into(),
                        instance_id.into(),
                    )]),
                },
            )
            .await
            {
                Ok(session_id) => session_id,
                Err(error) => {
                    boundary_denied(tx, error);
                    return;
                }
            };

            // Subscribe to bus for this session
            let Ok(mut rx) = ctx
                .bus
                .subscribe(SubscriptionFilter {
                    session_ids: Some(vec![sid.clone()]),
                    recipients: None,
                    kinds: None,
                })
                .await
            else {
                return;
            };

            let Ok(Some(session)) = ctx.sessions.get(&sid).await else {
                operation_error(tx, "chat", "authorized session is unavailable");
                return;
            };
            let _ = ctx
                .bus
                .publish(BusMessage::system_join_session(
                    agent_id.clone(),
                    sid.clone(),
                    session.metadata,
                ))
                .await;

            // Send user message
            let mut message = BusMessage::user_chat(sid.clone(), &principal.id.0, &text);
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
                match ui.discover_agents(&boundary).await {
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
                match ui.create_session(&boundary, request).await {
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
                    .session_config(&boundary, &SessionId::new(session_id))
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
                match ui.update_session_config(&boundary, request).await {
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
                match ui.submit_feedback(&boundary, feedback).await {
                    Ok(feedback_id) => {
                        let _ = tx.send(ServerMsg::FeedbackRecorded { feedback_id });
                    }
                    Err(error) => boundary_denied(tx, error),
                }
            } else {
                operation_error(tx, "submit_feedback", "UI service is unavailable");
            }
        }
        ClientMsg::SelectModel {
            session_id,
            model,
            reasoning_effort,
        } => {
            let Some(session_id) = session_id else {
                operation_error(tx, "select_model", "select_model requires a session_id");
                return;
            };
            let Some(ui) = &ctx.ui else {
                operation_error(tx, "select_model", "UI service is unavailable");
                return;
            };
            let session_id = SessionId::new(session_id);
            let state = match ui.session_config(&boundary, &session_id).await {
                Ok(state) => state,
                Err(error) => {
                    boundary_denied(tx, error);
                    return;
                }
            };
            let mut overrides = state.overrides;
            overrides.model_id = Some(model);
            overrides.reasoning_effort = Some(reasoning_effort);
            match ui
                .update_session_config(
                    &boundary,
                    sylvander_protocol::SessionConfigUpdateRequest {
                        session_id,
                        expected_revision: state.revision,
                        overrides,
                    },
                )
                .await
            {
                Ok(state) => {
                    let _ = tx.send(ServerMsg::SessionConfig { state });
                }
                Err(error) => boundary_denied(tx, error),
            }
        }
        ClientMsg::SelectPermissions {
            session_id,
            profile,
        } => {
            let Some(session_id) = session_id else {
                operation_error(
                    tx,
                    "select_permissions",
                    "select_permissions requires a session_id",
                );
                return;
            };
            let Some(ui) = &ctx.ui else {
                operation_error(tx, "select_permissions", "UI service is unavailable");
                return;
            };
            let session_id = SessionId::new(session_id);
            let state = match ui.session_config(&boundary, &session_id).await {
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
                    &boundary,
                    sylvander_protocol::SessionConfigUpdateRequest {
                        session_id,
                        expected_revision: state.revision,
                        overrides,
                    },
                )
                .await
            {
                Ok(state) => {
                    let _ = tx.send(ServerMsg::SessionConfig { state });
                }
                Err(error) => boundary_denied(tx, error),
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

fn boundary_denied(
    tx: &mpsc::UnboundedSender<ServerMsg>,
    error: sylvander_protocol::BoundaryError,
) {
    let _ = tx.send(ServerMsg::BoundaryDenied { error });
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_agent::bus::InProcessMessageBus;
    use sylvander_agent::session_store::{SessionStore, SqliteSessionStore};
    use sylvander_channel::UiService;

    struct DenyAgentAccess;

    struct SessionConfigUi {
        states: Mutex<HashMap<String, sylvander_protocol::SessionConfigState>>,
    }

    fn config_state(id: &str) -> sylvander_protocol::SessionConfigState {
        use sylvander_protocol::{
            SessionConfigProvenance, SessionConfigSource, SessionConfigSourceKind,
            SessionEffectiveConfig,
        };
        let source = SessionConfigSource {
            kind: SessionConfigSourceKind::AgentDefault,
            reference: Some("agent-1".into()),
        };
        sylvander_protocol::SessionConfigState {
            session_id: SessionId::new(id),
            revision: 1,
            overrides: sylvander_protocol::SessionConfigOverrides::default(),
            effective: SessionEffectiveConfig {
                agent_id: AgentId::new("agent-1"),
                agent_revision: 1,
                provider_id: "test".into(),
                model_id: "default-model".into(),
                reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
                permissions: sylvander_protocol::PermissionProfile::default(),
                prompt_profile: None,
                system_prompt_sha256: "digest".into(),
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

    #[async_trait]
    impl UiService for SessionConfigUi {
        async fn authorize_message(
            &self,
            _: &sylvander_protocol::BoundaryContext,
            _: &ClientMsg,
        ) -> Result<(), sylvander_protocol::BoundaryError> {
            Ok(())
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
            unreachable!()
        }

        async fn session_config(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            session_id: &SessionId,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
            self.states
                .lock()
                .await
                .get(&session_id.0)
                .cloned()
                .ok_or_else(|| {
                    sylvander_protocol::BoundaryError::forbidden(boundary, "get_session_config")
                })
        }

        async fn update_session_config(
            &self,
            boundary: &sylvander_protocol::BoundaryContext,
            request: sylvander_protocol::SessionConfigUpdateRequest,
        ) -> Result<sylvander_protocol::SessionConfigState, sylvander_protocol::BoundaryError>
        {
            let mut states = self.states.lock().await;
            let state = states.get_mut(&request.session_id.0).ok_or_else(|| {
                sylvander_protocol::BoundaryError::forbidden(boundary, "update_session_config")
            })?;
            assert_eq!(request.expected_revision, state.revision);
            state.revision += 1;
            state.overrides = request.overrides;
            if let Some(model) = &state.overrides.model_id {
                state.effective.model_id = model.clone();
            }
            if let Some(effort) = state.overrides.reasoning_effort {
                state.effective.reasoning_effort = effort;
            }
            if let Some(profile) = &state.overrides.permissions {
                state.effective.permissions = profile.clone();
            }
            Ok(state.clone())
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
    fn message_limit_is_configurable() {
        let channel =
            WsChannel::new("127.0.0.1:0".parse().unwrap(), "agent").with_request_limit(4096);
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

    #[test]
    fn bearer_comparison_checks_content_and_length() {
        assert!(constant_time_eq(b"correct-token", b"correct-token"));
        assert!(!constant_time_eq(b"correct-token", b"wrong-token"));
        assert!(!constant_time_eq(b"token", b"token-extra"));
    }

    #[tokio::test]
    async fn first_chat_cannot_create_a_session_without_agent_access() {
        let sessions: Arc<dyn SessionStore> =
            Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
        let context = ChannelContext {
            bus: Arc::new(InProcessMessageBus::new()),
            sessions: sessions.clone(),
            ui: Some(Arc::new(DenyAgentAccess)),
            readiness: None,
        };
        let principal = sylvander_protocol::AuthenticatedPrincipal::user(
            "caller",
            sylvander_protocol::AuthenticationMethod::BearerToken,
        );
        let (tx, mut rx) = mpsc::unbounded_channel();

        handle_client_msg(
            ClientMsg::Chat {
                text: "hello".into(),
                attachments: Vec::new(),
                session_id: None,
                workspace: None,
            },
            &context,
            &AgentId::new("private-agent"),
            &tx,
            &principal,
            "ws-private",
        )
        .await;

        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::BoundaryDenied { error })
                if error.code == sylvander_protocol::BoundaryErrorCode::Forbidden
        ));
        assert!(sessions.list_persistent().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn authentication_rejection_uses_runtime_status() {
        let state = AppState {
            ctx: Arc::new(ChannelContext {
                bus: Arc::new(InProcessMessageBus::new()),
                sessions: Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
                ui: Some(Arc::new(DenyAgentAccess)),
                readiness: None,
            }),
            agent_id: AgentId::new("private-agent"),
            clients: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(0)),
            instance_id: "ws-private".into(),
            auth: None,
            max_request_bytes: 4096,
        };
        assert_eq!(
            reject_ws_authentication(&state).await,
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn selection_updates_only_the_addressed_session() {
        let ui = Arc::new(SessionConfigUi {
            states: Mutex::new(HashMap::from([
                ("session-a".into(), config_state("session-a")),
                ("session-b".into(), config_state("session-b")),
            ])),
        });
        let context = ChannelContext {
            bus: Arc::new(InProcessMessageBus::new()),
            sessions: Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
            ui: Some(ui.clone()),
            readiness: None,
        };
        let principal = sylvander_protocol::AuthenticatedPrincipal::user(
            "caller",
            sylvander_protocol::AuthenticationMethod::BearerToken,
        );
        let (tx, mut rx) = mpsc::unbounded_channel();

        handle_client_msg(
            ClientMsg::SelectModel {
                session_id: Some("session-a".into()),
                model: "thinking-model".into(),
                reasoning_effort: sylvander_protocol::ReasoningEffort::High,
            },
            &context,
            &AgentId::new("agent-1"),
            &tx,
            &principal,
            "ws-test",
        )
        .await;

        assert!(matches!(
            rx.recv().await,
            Some(ServerMsg::SessionConfig { state })
                if state.session_id.0 == "session-a"
                    && state.effective.model_id == "thinking-model"
                    && state.effective.reasoning_effort
                        == sylvander_protocol::ReasoningEffort::High
        ));
        let states = ui.states.lock().await;
        assert_eq!(states["session-a"].revision, 2);
        assert_eq!(states["session-b"], config_state("session-b"));
    }
}
