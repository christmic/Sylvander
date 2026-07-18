//! HTTP debug channel — curl-friendly API with SSE streaming.
//!
//! ```bash
//! curl -N -X POST http://localhost:8080/chat \
//!   -H 'Content-Type: application/json' \
//!   -H "Authorization: Bearer ${SYLVANDER_HTTP_TOKEN}" \
//!   -d '{"session_id":"test","message":"hello"}'
//! ```

use std::collections::BTreeMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::Mutex;

use sylvander_agent::bus::{MessageKind, StreamEvent};
use sylvander_agent::spec::SessionId;
use sylvander_channel::credential::{
    CredentialLeaseError, CredentialLeaseRequest, CredentialLeaseSource,
};
use sylvander_channel::{Channel, ChannelContext, ExternalChatRequest, submit_external_chat};

#[derive(Deserialize)]
struct ChatRequest {
    session_id: String,
    message: String,
}

/// Authenticated HTTP/SSE adapter for bounded debug and automation traffic.
pub struct HttpChannel {
    addr: SocketAddr,
    agent_id: sylvander_agent::spec::AgentId,
    instance_id: String,
    principal_id: Option<String>,
    bearer_lease: Option<BearerLease>,
    max_request_bytes: usize,
    operational_health: Option<OperationalHealthProvider>,
}

/// Boxed future returned by an operational-health provider.
pub type OperationalHealthFuture =
    Pin<Box<dyn Future<Output = Result<OperationalHealth, String>> + Send>>;
/// Runtime-owned callback that supplies a fresh operational snapshot.
pub type OperationalHealthProvider =
    Arc<dyn Fn() -> OperationalHealthFuture + Send + Sync + 'static>;

/// Content-safe health and message-bus counters exposed by operational routes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationalHealth {
    /// Whether Runtime currently satisfies its readiness contract.
    pub ready: bool,
    /// Number of configured Agent definitions.
    pub agents: usize,
    /// Number of durable sessions.
    pub persistent_sessions: usize,
    /// Number of process-local sessions.
    pub ephemeral_sessions: usize,
    /// Number of channel instances that reported ready.
    pub ready_channels: usize,
    /// Total number of supervised channel instances.
    pub total_channels: usize,
    /// Current message-bus subscriber count.
    pub bus_subscribers: usize,
    /// Configured per-subscription message capacity.
    pub bus_capacity: usize,
    /// Cumulative accepted message count.
    pub published_messages: u64,
    /// Cumulative publish attempts rejected by backpressure.
    pub backpressure_rejections: u64,
}

impl HttpChannel {
    /// Construct an adapter bound to `addr` and one configured Agent.
    pub fn new(addr: SocketAddr, agent_id: impl Into<sylvander_agent::spec::AgentId>) -> Self {
        Self {
            addr,
            agent_id: agent_id.into(),
            instance_id: "http".into(),
            principal_id: None,
            bearer_lease: None,
            max_request_bytes: 1024 * 1024,
            operational_health: None,
        }
    }

    /// Bound the decoded request body before JSON parsing.
    #[must_use]
    pub const fn with_request_limit(mut self, max_request_bytes: usize) -> Self {
        self.max_request_bytes = max_request_bytes;
        self
    }

    /// Require a renewable bearer lease and bind accepted requests to
    /// `principal_id`.
    pub fn with_bearer_lease(
        mut self,
        instance_id: impl Into<String>,
        principal_id: impl Into<String>,
        source: Arc<dyn CredentialLeaseSource>,
    ) -> Result<Self, CredentialLeaseError> {
        self.instance_id = instance_id.into();
        self.principal_id = Some(principal_id.into());
        self.bearer_lease = Some(BearerLease {
            request: CredentialLeaseRequest::new(self.instance_id.clone(), ["bearer_token"])?,
            source,
        });
        Ok(self)
    }

    #[must_use]
    /// Attach Runtime's live operational-health provider.
    pub fn with_operational_health(mut self, provider: OperationalHealthProvider) -> Self {
        self.operational_health = Some(provider);
        self
    }
}

#[async_trait]
impl Channel for HttpChannel {
    fn name(&self) -> &'static str {
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
            bearer_lease: self.bearer_lease.clone(),
            operational_health: self.operational_health.clone(),
        });

        let chat_routes =
            Router::new()
                .route("/chat", post(chat))
                .route_layer(middleware::from_fn_with_state(
                    state.clone(),
                    require_http_authentication,
                ));
        let app = Router::new()
            .route("/health", get(health))
            .route("/ready", get(readiness))
            .route("/metrics", get(metrics))
            .merge(chat_routes)
            .layer(DefaultBodyLimit::max(self.max_request_bytes))
            .with_state(state.clone());

        let listener = match tokio::net::TcpListener::bind(self.addr).await {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(%error, addr = %self.addr, "http channel bind failed");
                return;
            }
        };
        tracing::info!(addr = %self.addr, "http channel listening");
        state.ctx.mark_ready();
        let shutdown = state.ctx.clone();
        if let Err(error) = axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.shutdown_requested().await })
            .await
        {
            tracing::warn!(%error, "http channel server failed");
        }
    }
}

async fn require_http_authentication(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(principal) = authenticate(&state, &headers).await else {
        return reject_http_authentication(&state).await.into_response();
    };
    request.extensions_mut().insert(principal);
    next.run(request).await
}

async fn reject_http_authentication(state: &AppState) -> StatusCode {
    let boundary = sylvander_protocol::BoundaryContext::unauthenticated(
        &state.instance_id,
        "http",
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
        boundary_status(error)
    } else {
        StatusCode::UNAUTHORIZED
    }
}

struct AppState {
    ctx: Arc<ChannelContext>,
    agent_id: sylvander_agent::spec::AgentId,
    sessions: Mutex<std::collections::HashMap<String, SessionId>>,
    instance_id: String,
    principal_id: Option<String>,
    bearer_lease: Option<BearerLease>,
    operational_health: Option<OperationalHealthProvider>,
}

async fn health(State(state): State<Arc<AppState>>) -> Response {
    operational_health(&state, false).await
}

async fn readiness(State(state): State<Arc<AppState>>) -> Response {
    operational_health(&state, true).await
}

async fn operational_health(state: &AppState, readiness_only: bool) -> Response {
    let Some(provider) = &state.operational_health else {
        return Json(serde_json::json!({"status":"ok","ready":true})).into_response();
    };
    match provider().await {
        Ok(snapshot) => {
            let status = if snapshot.ready {
                StatusCode::OK
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            if readiness_only {
                return (status, Json(serde_json::json!({"ready": snapshot.ready})))
                    .into_response();
            }
            (
                status,
                Json(serde_json::json!({"status": if snapshot.ready {"ok"} else {"degraded"}, "runtime": snapshot})),
            )
                .into_response()
        }
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"status":"unavailable","ready":false})),
        )
            .into_response(),
    }
}

async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    let Some(provider) = &state.operational_health else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(snapshot) = provider().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let ready = u8::from(snapshot.ready);
    let body = format!(
        "sylvander_ready {ready}\n\
         sylvander_agents {}\n\
         sylvander_sessions{{lifetime=\"persistent\"}} {}\n\
         sylvander_sessions{{lifetime=\"ephemeral\"}} {}\n\
         sylvander_channels{{status=\"ready\"}} {}\n\
         sylvander_channels_total {}\n\
         sylvander_bus_subscribers {}\n\
         sylvander_bus_subscription_capacity {}\n\
         sylvander_bus_published_messages_total {}\n\
         sylvander_bus_backpressure_rejections_total {}\n",
        snapshot.agents,
        snapshot.persistent_sessions,
        snapshot.ephemeral_sessions,
        snapshot.ready_channels,
        snapshot.total_channels,
        snapshot.bus_subscribers,
        snapshot.bus_capacity,
        snapshot.published_messages,
        snapshot.backpressure_rejections
    );
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

async fn chat(
    State(state): State<Arc<AppState>>,
    Extension(principal): Extension<sylvander_protocol::AuthenticatedPrincipal>,
    Json(req): Json<ChatRequest>,
) -> Result<
    Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>>,
    StatusCode,
> {
    let mut aliases = state.sessions.lock().await;
    let existing_session = aliases.get(&req.session_id).cloned();
    let boundary = sylvander_protocol::BoundaryContext::authenticated(
        principal,
        &state.instance_id,
        "http",
        uuid::Uuid::new_v4().to_string(),
    );
    let submitted = submit_external_chat(
        &state.ctx,
        &boundary,
        ExternalChatRequest {
            existing_session: existing_session.clone(),
            agent_id: state.agent_id.clone(),
            label: "HTTP session".into(),
            overrides: sylvander_protocol::SessionConfigOverrides::default(),
            text: req.message.clone(),
            attachments: Vec::new(),
            external_meta: BTreeMap::from([
                ("channel_instance_id".into(), state.instance_id.clone()),
                ("http_session_key".into(), req.session_id.clone()),
            ]),
        },
    )
    .await
    .map_err(boundary_status)?;
    let sid = submitted.session_id;
    let mut event_rx = submitted.events;
    if existing_session.is_none() {
        aliases.insert(req.session_id.clone(), sid.clone());
    }
    drop(aliases);

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
                    StreamEvent::Error { message } => {
                        yield Ok(Event::default().data(message.as_str()).event("error"));
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

fn boundary_status(error: sylvander_protocol::BoundaryError) -> StatusCode {
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
    let supplied = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let lease = state.bearer_lease.as_ref()?;
    let leased = lease.source.lease(&lease.request).await.ok()?;
    if !leased.contains_exact_slots(&lease.request.slots) {
        return None;
    }
    let expected = leased.secret("bearer_token").ok()?;
    if !constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
        return None;
    }
    Some(sylvander_protocol::AuthenticatedPrincipal::user(
        state.principal_id.clone()?,
        sylvander_protocol::AuthenticationMethod::BearerToken,
    ))
}

#[derive(Clone)]
struct BearerLease {
    source: Arc<dyn CredentialLeaseSource>,
    request: CredentialLeaseRequest,
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
#[path = "../tests/unit/lib.rs"]
mod tests;
