//! WebSocket channel — desktop client integration.
//!
//! # Protocol
//!
//! JSON messages over a single WebSocket connection, full-duplex.
//!
//! ## Client → Server (commands)
//! ```json
//! {"type":"hello","protocol":{"client_name":"example","min_version":4,"max_version":4,"capabilities":[]}}
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

use sylvander_agent::bus::{MessageKind, StreamEvent};
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::credential::{
    CredentialLeaseError, CredentialLeaseRequest, CredentialLeaseSource,
};
use sylvander_channel::{
    Channel, ChannelContext, ExternalChatRequest, submit_external_chat,
    unavailable_agent_admin_response, unavailable_registry_admin_response,
};
use sylvander_protocol::{
    UiClientMessage as ClientMsg, UiServerMessage as ServerMsg, UiToolInfo as ToolInfo,
};

// ===========================================================================
// Channel
// ===========================================================================

/// Authenticated JSON-over-WebSocket adapter for desktop clients.
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
    bearer_lease: BearerLease,
}

#[derive(Clone)]
struct BearerLease {
    source: Arc<dyn CredentialLeaseSource>,
    request: CredentialLeaseRequest,
}

impl WsChannel {
    /// Construct an adapter bound to `addr` and one configured Agent.
    pub fn new(addr: SocketAddr, agent_id: impl Into<AgentId>) -> Self {
        Self {
            addr,
            agent_id: agent_id.into(),
            instance_id: "websocket".into(),
            auth: None,
            max_request_bytes: 1024 * 1024,
        }
    }

    /// Bound both WebSocket frames and assembled messages.
    #[must_use]
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }

    /// Require a renewable bearer lease and bind accepted upgrades to
    /// `principal_id`.
    pub fn with_bearer_lease(
        mut self,
        instance_id: impl Into<String>,
        principal_id: impl Into<String>,
        source: Arc<dyn CredentialLeaseSource>,
    ) -> Result<Self, CredentialLeaseError> {
        self.instance_id = instance_id.into();
        self.auth = Some(WsAuth {
            principal_id: principal_id.into(),
            bearer_lease: BearerLease {
                source,
                request: CredentialLeaseRequest::new(self.instance_id.clone(), ["bearer_token"])?,
            },
        });
        Ok(self)
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

        let listener = match tokio::net::TcpListener::bind(self.addr).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, addr = %self.addr, "ws channel bind failed");
                return;
            }
        };
        info!(addr = %self.addr, "ws channel listening");
        state.ctx.mark_ready();
        let shutdown = state.ctx.clone();
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.shutdown_requested().await })
            .await
        {
            tracing::warn!(%error, "ws channel server failed");
        }
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
    let Some(principal) = authenticate(&state, &headers).await else {
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

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<sylvander_protocol::AuthenticatedPrincipal> {
    let auth = state.auth.as_ref()?;
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let lease = &auth.bearer_lease;
    let leased = lease.source.lease(&lease.request).await.ok()?;
    if !leased.contains_exact_slots(&lease.request.slots) {
        return None;
    }
    let expected = leased.secret("bearer_token").ok()?;
    constant_time_eq(supplied.as_bytes(), expected.as_bytes()).then(|| {
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
    let mut selected_protocol = None;
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
        if !handle_protocol_message(
            parsed,
            &mut selected_protocol,
            &ctx,
            &agent_id,
            &tx,
            &principal,
            &state.instance_id,
        )
        .await
        {
            break;
        }
    }

    // Cleanup
    write_task.abort();
    clients.lock().await.remove(&client_id_for_cleanup);
    info!(client_id = client_id_for_cleanup, "ws client disconnected");
}

async fn handle_protocol_message(
    msg: ClientMsg,
    selected: &mut Option<u16>,
    ctx: &ChannelContext,
    agent_id: &AgentId,
    tx: &mpsc::UnboundedSender<ServerMsg>,
    principal: &sylvander_protocol::AuthenticatedPrincipal,
    instance_id: &str,
) -> bool {
    match (&msg, *selected) {
        (ClientMsg::Hello { protocol }, None) => {
            match sylvander_protocol::negotiate_ui_protocol(protocol) {
                Ok(version) => {
                    *selected = Some(version);
                    send_welcome(tx, version);
                    true
                }
                Err(error) => {
                    let _ = tx.send(ServerMsg::ProtocolError { error });
                    false
                }
            }
        }
        (ClientMsg::Hello { .. }, Some(_)) => {
            send_protocol_error(
                tx,
                "duplicate_handshake",
                "connection is already negotiated",
            );
            true
        }
        (_, None) => {
            send_protocol_error(
                tx,
                "handshake_required",
                "hello must be the first client message",
            );
            false
        }
        (ClientMsg::RegistryAdmin { request }, Some(version))
            if request.minimum_ui_protocol_version() > version =>
        {
            send_protocol_error(
                tx,
                "unsupported_message_version",
                "message requires a newer UI protocol version",
            );
            true
        }
        (_, Some(_)) => {
            handle_client_msg(msg, ctx, agent_id, tx, principal, instance_id).await;
            true
        }
    }
}

fn send_welcome(tx: &mpsc::UnboundedSender<ServerMsg>, version: u16) {
    let mut capabilities = vec![
        "agent_discovery".into(),
        sylvander_protocol::IDENTITY_BINDING_CAPABILITY.into(),
        "session_config".into(),
        "sessions".into(),
        "feedback".into(),
        sylvander_protocol::USER_PROFILE_CAPABILITY.into(),
    ];
    if version >= 2 {
        capabilities.extend([
            "agent_administration".into(),
            "registry_administration".into(),
        ]);
    }
    if version >= 3 {
        capabilities.push("credential_registry_lifecycle".into());
        capabilities.push("provider_model_registry_lifecycle".into());
    }
    let _ = tx.send(ServerMsg::Welcome {
        protocol: sylvander_protocol::UiProtocolWelcome {
            server_name: "sylvander-server".into(),
            version,
            capabilities,
        },
    });
}

fn send_protocol_error(tx: &mpsc::UnboundedSender<ServerMsg>, code: &str, message: &str) {
    let _ = tx.send(ServerMsg::ProtocolError {
        error: sylvander_protocol::UiProtocolError {
            code: code.into(),
            message: message.into(),
            server_min_version: sylvander_protocol::UI_PROTOCOL_MIN_VERSION,
            server_max_version: sylvander_protocol::UI_PROTOCOL_MAX_VERSION,
        },
    });
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
                send_welcome(tx, version);
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
            let submitted = match submit_external_chat(
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
                Ok(submitted) => submitted,
                Err(error) => {
                    boundary_denied(tx, error);
                    return;
                }
            };
            let sid = submitted.session_id;
            let mut rx = submitted.events;

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
            if let Err(error) = ctx
                .submit_control(
                    &boundary,
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
                    &boundary,
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
        ClientMsg::AgentAdmin { request } => {
            let response = if let Some(ui) = &ctx.ui {
                ui.agent_admin(&boundary, request).await
            } else {
                unavailable_agent_admin_response()
            };
            let _ = tx.send(ServerMsg::AgentAdmin { response });
        }
        ClientMsg::RegistryAdmin { request } => {
            let response = if let Some(ui) = &ctx.ui {
                ui.registry_admin(&boundary, request).await
            } else {
                unavailable_registry_admin_response()
            };
            let _ = tx.send(ServerMsg::RegistryAdmin { response });
        }
        ClientMsg::UserProfile { request } => {
            let response = if let Some(ui) = &ctx.ui {
                ui.user_profile(&boundary, request).await
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
                Some(request) => ctx.submit_identity_binding(&boundary, request).await,
                None => sylvander_protocol::IdentityBindingResponse::Error {
                    version: sylvander_protocol::IDENTITY_BINDING_PROTOCOL_VERSION,
                    error: sylvander_protocol::IdentityBindingError::service_unavailable(operation),
                },
            };
            let _ = tx.send(ServerMsg::IdentityBinding {
                response: Arc::new(response),
            });
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
            let agents = match ui.discover_agents(&boundary).await {
                Ok(agents) => agents,
                Err(error) => {
                    boundary_denied(tx, error);
                    return;
                }
            };
            let catalog = visible_model_catalog(&agents, &state.effective.agent_id);
            if !catalog.contains(&model) {
                operation_error(
                    tx,
                    "select_model",
                    format!(
                        "model selection `{}/{}` is unavailable",
                        model.provider_id, model.model_id
                    ),
                );
                return;
            }
            overrides.model = Some(model);
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
            if let Some(ui) = &ctx.ui {
                match ui.list_sessions(&boundary).await {
                    Ok(sessions) => {
                        let _ = tx.send(ServerMsg::SessionsList { sessions });
                    }
                    Err(error) => boundary_denied(tx, error),
                }
            } else {
                operation_error(tx, "list_sessions", "UI service is unavailable");
            }
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
#[path = "../tests/unit/lib.rs"]
mod tests;
