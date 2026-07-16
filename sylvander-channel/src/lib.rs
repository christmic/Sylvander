//! # sylvander-channel
//!
//! Abstract [`Channel`] trait — the contract between the agent system
//! and external communication channels (TUI, Telegram, HTTP API, ...).
//!
//! Channel implementations depend ONLY on this crate (+ `sylvander-agent`
//! for message types). They do NOT depend on `sylvander-runtime`.
//!
//! # Responsibilities
//!
//! A channel is responsible for:
//! 1. Receiving messages in its native protocol
//! 2. Extracting protocol metadata → storing in session
//! 3. Mapping external identifiers → internal [`SessionId`]
//! 4. Publishing normalized [`BusMessage`]s to the bus
//! 5. Rendering bus events (streaming text, tool calls, approvals)
//!    in channel-native format
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │  sylvander-channel-tui / telegram / http     │  ← implementations
//! ├──────────────────────────────────────────────┤
//! │  sylvander-channel  (this crate)              │  ← Channel trait
//! ├──────────────────────────────────────────────┤
//! │  sylvander-agent    (bus, session_store)      │  ← agent types
//! └──────────────────────────────────────────────┘
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use sylvander_agent::bus::{BusError, BusMessage, MessageBus, SubscriptionFilter};
use sylvander_agent::session_store::SessionStore;
use sylvander_protocol::{
    AgentAdminError, AgentAdminErrorCode, AgentAdminRequest, AgentAdminResponse, AgentDescriptor,
    AgentId, AuthenticationFailure, BoundaryContext, BoundaryError, BoundaryErrorCode,
    RegistryAdminError, RegistryAdminErrorCode, RegistryAdminRequest, RegistryAdminResponse,
    RunFeedback, SessionConfigOverrides, SessionConfigState, SessionConfigUpdateRequest,
    SessionCreateRequest, SessionId, UiClientMessage,
};

/// Complete normalized input for one authenticated external chat turn.
pub struct ExternalChatRequest {
    pub existing_session: Option<SessionId>,
    pub agent_id: AgentId,
    pub label: String,
    pub overrides: SessionConfigOverrides,
    pub text: String,
    pub attachments: Vec<sylvander_protocol::MessageAttachment>,
    pub external_meta: BTreeMap<String, String>,
}

/// Result of an authenticated chat submission.
///
/// The runtime subscribes before publishing the user message, so a transport
/// cannot miss the first response event while installing its relay.
#[derive(Debug)]
pub struct SubmittedChat {
    pub session_id: SessionId,
    pub events: tokio::sync::mpsc::UnboundedReceiver<BusMessage>,
}

/// Transport-neutral UI service boundary owned by the runtime.
#[async_trait]
pub trait UiService: Send + Sync {
    /// Reject an ingress request that failed authentication before a public
    /// message existed. Production runtimes override this to rate-limit and
    /// persist a content-free audit fact.
    async fn reject_authentication(
        &self,
        boundary: &BoundaryContext,
        _failure: AuthenticationFailure,
    ) -> BoundaryError {
        BoundaryError::unauthenticated(boundary, "authenticate")
    }

    /// Authorize one complete public operation before a transport dispatches it.
    async fn authorize_message(
        &self,
        boundary: &BoundaryContext,
        message: &UiClientMessage,
    ) -> Result<(), BoundaryError>;
    async fn discover_agents(
        &self,
        boundary: &BoundaryContext,
    ) -> Result<Vec<AgentDescriptor>, BoundaryError>;
    async fn create_session(
        &self,
        boundary: &BoundaryContext,
        request: SessionCreateRequest,
    ) -> Result<SessionConfigState, BoundaryError>;
    async fn session_config(
        &self,
        boundary: &BoundaryContext,
        session_id: &SessionId,
    ) -> Result<SessionConfigState, BoundaryError>;
    async fn update_session_config(
        &self,
        boundary: &BoundaryContext,
        request: SessionConfigUpdateRequest,
    ) -> Result<SessionConfigState, BoundaryError>;
    async fn submit_feedback(
        &self,
        boundary: &BoundaryContext,
        feedback: RunFeedback,
    ) -> Result<String, BoundaryError>;

    /// Authenticate, resolve or create and attach the owned session, then
    /// publish exactly one user chat message through the runtime boundary.
    async fn submit_chat(
        &self,
        boundary: &BoundaryContext,
        _request: ExternalChatRequest,
    ) -> Result<SubmittedChat, BoundaryError> {
        Err(BoundaryError {
            code: BoundaryErrorCode::InvalidScope,
            operation: "submit_chat".into(),
            request_id: boundary.request_id.clone(),
            message: "authenticated chat submission is unavailable".into(),
            retry_after_ms: None,
        })
    }

    /// Authorize and dispatch one session-scoped interactive control through
    /// the runtime. Chat submission has a separate atomic operation.
    async fn submit_control(
        &self,
        boundary: &BoundaryContext,
        _message: UiClientMessage,
    ) -> Result<(), BoundaryError> {
        Err(BoundaryError {
            code: BoundaryErrorCode::InvalidScope,
            operation: "submit_control".into(),
            request_id: boundary.request_id.clone(),
            message: "authenticated control submission is unavailable".into(),
            retry_after_ms: None,
        })
    }

    /// Apply one privileged Agent registry operation.
    ///
    /// Runtimes that have not installed an administration service fail closed.
    async fn agent_admin(
        &self,
        _boundary: &BoundaryContext,
        _request: AgentAdminRequest,
    ) -> AgentAdminResponse {
        unavailable_agent_admin_response()
    }

    /// Apply one privileged registry administration request.
    ///
    /// Runtimes that have not installed a registry service fail closed.
    async fn registry_admin(
        &self,
        _boundary: &BoundaryContext,
        _request: RegistryAdminRequest,
    ) -> RegistryAdminResponse {
        unavailable_registry_admin_response()
    }
}

/// Content-free response used when the runtime has no Agent administration
/// service. The request is intentionally not reflected into the response.
#[must_use]
pub fn unavailable_agent_admin_response() -> AgentAdminResponse {
    AgentAdminResponse::Error {
        error: AgentAdminError {
            code: AgentAdminErrorCode::Unauthorized,
            message: "Agent administration service is unavailable".into(),
            agent_id: None,
            revision: None,
            expected_active_revision: None,
            actual_active_revision: None,
        },
    }
}

/// Content-free response used when no registry administration service exists.
/// The request identity and revision are intentionally not reflected.
#[must_use]
pub fn unavailable_registry_admin_response() -> RegistryAdminResponse {
    RegistryAdminResponse::Error {
        error: RegistryAdminError {
            code: RegistryAdminErrorCode::Unauthorized,
            message: "Registry administration service is unavailable".into(),
            provider_id: None,
            model_id: None,
            binding_id_sha256: None,
            revision: None,
            generation: None,
            details: None,
        },
    }
}

// ---------------------------------------------------------------------------
// Channel
// ---------------------------------------------------------------------------

/// An external communication channel — the interface between the agent
/// system and a specific protocol (TUI, Telegram, HTTP, ...).
///
/// # Lifecycle
///
/// 1. The runtime creates the channel (e.g. `TelegramChannel::new(token)`)
/// 2. Calls [`Channel::run`] with a [`ChannelContext`]
/// 3. The channel starts its event loop, communicating exclusively
///    through the bus and session store
/// 4. On shutdown, the runtime requests a cooperative drain and waits for the
///    channel to stop
///
/// # Contract
///
/// - The channel MUST NOT call engine or agent methods directly
/// - All communication flows through the bus
/// - Session mapping (external ID → `SessionId`) is the channel's
///   responsibility, using the session store's metadata
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable channel name (for logging).
    fn name(&self) -> &str;

    /// Start the channel's event loop.
    ///
    /// The channel should:
    /// - Listen for external messages (stdin, webhook, polling, ...)
    /// - Subscribe to the bus for agent events
    /// - Map external IDs → session IDs via [`ChannelContext::sessions`]
    /// - Publish normalized messages via [`ChannelContext::bus`]
    ///
    /// Runs until the tokio task is cancelled or the channel decides
    /// to shut down.
    async fn run(self: Arc<Self>, ctx: ChannelContext);
}

// ---------------------------------------------------------------------------
// ChannelContext
// ---------------------------------------------------------------------------

/// Capabilities provided to a channel by the agent system.
///
/// The channel uses these to interact with agents and sessions.
/// It never accesses `AgentRun`, Engine, or Runtime directly.
#[derive(Clone)]
pub struct ChannelContext {
    /// Event subscription is exposed through [`ChannelContext::subscribe`].
    /// Channels never receive a public bus publisher.
    bus: Arc<dyn MessageBus>,
    /// Session persistence and external-ID mapping.
    pub sessions: Arc<dyn SessionStore>,
    /// Runtime-owned UI application service. Channels adapt transports only.
    pub ui: Option<Arc<dyn UiService>>,
    /// Runtime-owned startup handshake. Channel implementations call
    /// [`ChannelContext::mark_ready`] only after external input can arrive.
    #[doc(hidden)]
    pub readiness: Option<ChannelReadiness>,
}

impl ChannelContext {
    #[must_use]
    pub fn new(bus: Arc<dyn MessageBus>, sessions: Arc<dyn SessionStore>) -> Self {
        Self {
            bus,
            sessions,
            ui: None,
            readiness: None,
        }
    }

    #[must_use]
    pub fn with_runtime_services(
        bus: Arc<dyn MessageBus>,
        sessions: Arc<dyn SessionStore>,
        ui: Arc<dyn UiService>,
        readiness: Option<ChannelReadiness>,
    ) -> Self {
        Self::with_services(bus, sessions, Some(ui), readiness)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn with_services(
        bus: Arc<dyn MessageBus>,
        sessions: Arc<dyn SessionStore>,
        ui: Option<Arc<dyn UiService>>,
        readiness: Option<ChannelReadiness>,
    ) -> Self {
        Self {
            bus,
            sessions,
            ui,
            readiness,
        }
    }

    /// Subscribe to runtime output without obtaining publish authority.
    pub async fn subscribe(
        &self,
        filter: SubscriptionFilter,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<BusMessage>, BusError> {
        self.bus.subscribe(filter).await
    }

    /// Submit one authenticated interactive control through the UI boundary.
    pub async fn submit_control(
        &self,
        boundary: &BoundaryContext,
        message: UiClientMessage,
    ) -> Result<(), BoundaryError> {
        let ui = self.ui.as_ref().ok_or_else(|| BoundaryError {
            code: BoundaryErrorCode::InvalidScope,
            operation: "submit_control".into(),
            request_id: boundary.request_id.clone(),
            message: "runtime authorization service is unavailable".into(),
            retry_after_ms: None,
        })?;
        ui.submit_control(boundary, message).await
    }

    pub fn mark_ready(&self) {
        if let Some(readiness) = &self.readiness {
            readiness.mark_ready();
        }
    }

    pub async fn shutdown_requested(&self) {
        if let Some(readiness) = &self.readiness {
            readiness.shutdown_requested().await;
        } else {
            std::future::pending::<()>().await;
        }
    }
}

/// Submit one authenticated external chat through the runtime boundary.
///
/// External adapters use this instead of writing a session and publishing a
/// message directly. It applies Agent access policy on creation and session
/// ownership, payload, and rate policy to every inbound chat message.
pub async fn submit_external_chat(
    context: &ChannelContext,
    boundary: &BoundaryContext,
    request: ExternalChatRequest,
) -> Result<SubmittedChat, BoundaryError> {
    let ui = context.ui.as_ref().ok_or_else(|| BoundaryError {
        code: BoundaryErrorCode::InvalidScope,
        operation: "external_chat".into(),
        request_id: boundary.request_id.clone(),
        message: "runtime authorization service is unavailable".into(),
        retry_after_ms: None,
    })?;

    ui.submit_chat(boundary, request).await
}

#[derive(Clone)]
pub struct ChannelReadiness {
    inner: Arc<ReadinessInner>,
}

struct ReadinessInner {
    ready: AtomicBool,
    notify: tokio::sync::Notify,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl ChannelReadiness {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ReadinessInner {
                ready: AtomicBool::new(false),
                notify: tokio::sync::Notify::new(),
                shutdown: tokio::sync::watch::channel(false).0,
            }),
        }
    }

    pub fn mark_ready(&self) {
        if !self.inner.ready.swap(true, Ordering::SeqCst) {
            self.inner.notify.notify_one();
        }
    }

    pub async fn wait(&self) {
        if !self.inner.ready.load(Ordering::SeqCst) {
            self.inner.notify.notified().await;
        }
    }

    pub fn request_shutdown(&self) {
        let _ = self.inner.shutdown.send(true);
    }

    pub async fn shutdown_requested(&self) {
        let mut shutdown = self.inner.shutdown.subscribe();
        if *shutdown.borrow() {
            return;
        }
        while shutdown.changed().await.is_ok() {
            if *shutdown.borrow() {
                return;
            }
        }
    }
}

impl Default for ChannelReadiness {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sylvander_agent::bus::InProcessMessageBus;
    use sylvander_agent::session_store::SqliteSessionStore;
    use sylvander_protocol::{AuthenticatedPrincipal, AuthenticationMethod};

    struct DefaultUiService;

    #[async_trait]
    impl UiService for DefaultUiService {
        async fn authorize_message(
            &self,
            boundary: &BoundaryContext,
            _: &UiClientMessage,
        ) -> Result<(), BoundaryError> {
            Err(BoundaryError::forbidden(boundary, "test"))
        }

        async fn discover_agents(
            &self,
            boundary: &BoundaryContext,
        ) -> Result<Vec<AgentDescriptor>, BoundaryError> {
            Err(BoundaryError::forbidden(boundary, "test"))
        }

        async fn create_session(
            &self,
            boundary: &BoundaryContext,
            _: SessionCreateRequest,
        ) -> Result<SessionConfigState, BoundaryError> {
            Err(BoundaryError::forbidden(boundary, "test"))
        }

        async fn session_config(
            &self,
            boundary: &BoundaryContext,
            _: &SessionId,
        ) -> Result<SessionConfigState, BoundaryError> {
            Err(BoundaryError::forbidden(boundary, "test"))
        }

        async fn update_session_config(
            &self,
            boundary: &BoundaryContext,
            _: SessionConfigUpdateRequest,
        ) -> Result<SessionConfigState, BoundaryError> {
            Err(BoundaryError::forbidden(boundary, "test"))
        }

        async fn submit_feedback(
            &self,
            boundary: &BoundaryContext,
            _: RunFeedback,
        ) -> Result<String, BoundaryError> {
            Err(BoundaryError::forbidden(boundary, "test"))
        }
    }

    #[tokio::test]
    async fn agent_admin_default_fails_closed_without_reflecting_request() {
        let boundary = BoundaryContext::unauthenticated("unix", "unix", "request-1");
        let response = DefaultUiService
            .agent_admin(
                &boundary,
                AgentAdminRequest::InspectRevision {
                    agent_id: AgentId::new("private-agent"),
                    revision: 42,
                },
            )
            .await;
        let json = serde_json::to_string(&response).expect("serialize response");

        assert!(matches!(
            response,
            AgentAdminResponse::Error {
                error: AgentAdminError {
                    code: AgentAdminErrorCode::Unauthorized,
                    agent_id: None,
                    revision: None,
                    ..
                }
            }
        ));
        assert!(!json.contains("private-agent"));
        assert!(!json.contains("42"));
    }

    #[tokio::test]
    async fn registry_admin_default_fails_closed_without_reflecting_request() {
        let boundary = BoundaryContext::unauthenticated("unix", "unix", "request-1");
        let response = DefaultUiService
            .registry_admin(
                &boundary,
                RegistryAdminRequest::InspectProviderRevision {
                    provider_id: "private-provider".into(),
                    revision: 42,
                },
            )
            .await;
        let json = serde_json::to_string(&response).expect("serialize response");

        assert!(matches!(
            response,
            RegistryAdminResponse::Error {
                error: RegistryAdminError {
                    code: RegistryAdminErrorCode::Unauthorized,
                    provider_id: None,
                    revision: None,
                    ..
                }
            }
        ));
        assert!(!json.contains("private-provider"));
        assert!(!json.contains("42"));
    }

    #[tokio::test]
    async fn external_chat_fails_closed_without_runtime_authorizer() {
        let context = ChannelContext::new(
            Arc::new(InProcessMessageBus::new()),
            Arc::new(SqliteSessionStore::open_in_memory().await.unwrap()),
        );
        let boundary = BoundaryContext::authenticated(
            AuthenticatedPrincipal::user(
                "telegram:bot-a:42",
                AuthenticationMethod::PlatformIdentity,
            ),
            "bot-a",
            "telegram",
            "update-1",
        );

        let error = submit_external_chat(
            &context,
            &boundary,
            ExternalChatRequest {
                existing_session: None,
                agent_id: AgentId::new("assistant"),
                label: "telegram-42".into(),
                overrides: SessionConfigOverrides::default(),
                text: "hello".into(),
                attachments: Vec::new(),
                external_meta: BTreeMap::new(),
            },
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, BoundaryErrorCode::InvalidScope);
    }
}
