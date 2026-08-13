//! # sylvander-channel
//!
//! Abstract [`Channel`] trait — the contract between the agent system
//! and external communication channels (TUI, Telegram, HTTP API, ...).
//!
//! Channel implementations depend on this contract and protocol DTOs. They do
//! not receive Runtime persistence backends or concrete Agent runs.
//!
//! # Responsibilities
//!
//! A channel is responsible for:
//! 1. Receiving messages in its native protocol
//! 2. Mapping native input into authenticated protocol DTOs
//! 3. Submitting operations to the runtime-owned [`ChannelHost`]
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
//! │  Runtime host ports + protocol DTOs            │  ← owned boundaries
//! └──────────────────────────────────────────────┘
//! ```

pub mod credential;
pub mod message_bus;

pub use message_bus::{
    BusDiagnostics, BusError, InProcessMessageBus, MessageBus, SubscriptionFilter,
};

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use sylvander_api::BusMessage;
use sylvander_api::{
    AgentAdminError, AgentAdminErrorCode, AgentAdminRequest, AgentAdminResponse, AgentDescriptor,
    AgentId, AuthenticationFailure, BoundaryContext, BoundaryError, BoundaryErrorCode,
    IDENTITY_BINDING_PROTOCOL_VERSION, IdentityBindingCapabilities, IdentityBindingError,
    IdentityBindingErrorCode, IdentityBindingOperation, IdentityBindingRequest,
    IdentityBindingResponse, IdentityBindingValidationError, MemoryConfirmationRequest,
    MemoryConfirmationResponse, PrincipalKind, RegistryAdminError, RegistryAdminErrorCode,
    RegistryAdminRequest, RegistryAdminResponse, RunFeedback, RuntimeUiSnapshot,
    SessionConfigOverrides, SessionConfigState, SessionConfigUpdateRequest, SessionCreateRequest,
    SessionId, USER_PROFILE_PROTOCOL_VERSION, UiClientMessage, UiSessionInfo,
    UserProfileCapabilities, UserProfileError, UserProfileRequest, UserProfileResponse,
};

/// Complete normalized input for one authenticated external chat turn.
pub struct ExternalChatRequest {
    /// Existing durable session selected by a transport-specific mapping.
    pub existing_session: Option<SessionId>,
    /// Requested Agent; Runtime reauthorizes access before use.
    pub agent_id: AgentId,
    /// Human-readable label used only when Runtime creates a session.
    pub label: String,
    /// Requested session configuration layered over channel defaults.
    pub overrides: SessionConfigOverrides,
    /// User message content.
    pub text: String,
    /// Typed UI attachments forwarded without flattening.
    pub attachments: Vec<sylvander_api::MessageAttachment>,
    /// Transport-owned identifiers used for future session lookup.
    pub external_meta: BTreeMap<String, String>,
}

/// Result of an authenticated chat submission.
///
/// The runtime subscribes before publishing the user message, so a transport
/// cannot miss the first response event while installing its relay.
#[derive(Debug)]
pub struct SubmittedChat {
    /// Runtime-authorized session receiving the turn.
    pub session_id: SessionId,
    /// Opaque handle for feedback about this exact submitted turn.
    pub feedback_target: Option<sylvander_api::FeedbackTarget>,
    /// Session-scoped event subscription installed before message publication.
    pub events: tokio::sync::mpsc::Receiver<BusMessage>,
}

/// Non-serializable identity derived inside an authenticated Channel ingress.
///
/// There is deliberately no public constructor. A wire client or model can
/// construct an identity-binding request, but only [`ChannelContext`] can bind
/// it to the transport principal established by a concrete adapter's
/// authentication path. Runtime code may consume the typed parts after it has
/// independently authorized the accompanying [`BoundaryContext`].
#[derive(Clone, PartialEq, Eq)]
pub struct AuthenticatedTransportIdentity {
    transport: String,
    channel_instance_id: String,
    external_principal_id: String,
}

impl AuthenticatedTransportIdentity {
    fn from_ingress(boundary: &BoundaryContext) -> Result<Self, IdentityIngressError> {
        let principal = boundary
            .principal
            .as_ref()
            .ok_or(IdentityIngressError::Unauthenticated)?;
        if principal.kind != PrincipalKind::User {
            return Err(IdentityIngressError::Forbidden);
        }
        validate_ingress_part(&boundary.transport)?;
        validate_ingress_part(&boundary.channel_instance_id)?;
        validate_ingress_part(&principal.id.0)?;
        Ok(Self {
            transport: boundary.transport.clone(),
            channel_instance_id: boundary.channel_instance_id.clone(),
            external_principal_id: principal.id.0.clone(),
        })
    }

    /// Consume the sealed envelope at the Runtime identity-store adapter.
    #[must_use]
    pub fn into_parts(self) -> (String, String, String) {
        (
            self.transport,
            self.channel_instance_id,
            self.external_principal_id,
        )
    }
}

impl std::fmt::Debug for AuthenticatedTransportIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticatedTransportIdentity")
            .field("transport", &self.transport)
            .field("channel_instance_id", &self.channel_instance_id)
            .field("external_principal_id", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityIngressError {
    Unauthenticated,
    Forbidden,
    Invalid,
}

fn validate_ingress_part(value: &str) -> Result<(), IdentityIngressError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        return Err(IdentityIngressError::Invalid);
    }
    Ok(())
}

/// Transport-neutral UI service boundary owned by the runtime.
#[async_trait]
pub trait ChannelHost: Send + Sync {
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
    /// List the sessions visible to the effective stable user represented by
    /// `boundary`.
    ///
    /// This operation belongs to the Runtime rather than the transport because
    /// a channel principal may resolve to a different stable `UserId`.
    async fn list_sessions(
        &self,
        boundary: &BoundaryContext,
        _include_archived: bool,
    ) -> Result<Vec<UiSessionInfo>, BoundaryError> {
        Err(BoundaryError {
            code: BoundaryErrorCode::InvalidScope,
            operation: "list_sessions".into(),
            request_id: boundary.request_id.clone(),
            message: "runtime-owned session discovery is unavailable".into(),
            retry_after_ms: None,
        })
    }
    /// Return live, redacted Runtime state for one visible Agent.
    ///
    /// The transport supplies the configured Agent identity but must not
    /// assemble model, permission, platform, or boundary-limit fields itself.
    async fn runtime_snapshot(
        &self,
        boundary: &BoundaryContext,
        _agent_id: &AgentId,
        _session_id: Option<&SessionId>,
    ) -> Result<RuntimeUiSnapshot, BoundaryError> {
        Err(unavailable_ui_control(boundary, "runtime_snapshot"))
    }
    /// Load one visible session and its durable transcript.
    async fn load_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_api::UiSessionHistory, BoundaryError> {
        Err(unavailable_ui_control(boundary, "load_session"))
    }
    /// Rename one visible session without exposing persistence metadata.
    async fn rename_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
        _label: String,
    ) -> Result<(), BoundaryError> {
        Err(unavailable_ui_control(boundary, "rename_session"))
    }
    /// Archive one visible session through the Runtime lifecycle.
    async fn archive_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<(), BoundaryError> {
        Err(unavailable_ui_control(boundary, "archive_session"))
    }
    /// Restore one owned archived session through the Runtime lifecycle.
    async fn restore_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<(), BoundaryError> {
        Err(unavailable_ui_control(boundary, "restore_session"))
    }
    /// Branch durable conversation history into a new Runtime session.
    async fn fork_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
        _completed_turns: Option<usize>,
        _checkpoint: bool,
    ) -> Result<sylvander_api::UiSessionHistory, BoundaryError> {
        Err(unavailable_ui_control(boundary, "fork_session"))
    }
    /// Resolve one channel-native identity to a visible Runtime Session.
    ///
    /// Runtime binds `channel_instance_id` when constructing the context and
    /// must require every selector to match exactly. The default fails closed.
    async fn resolve_external_session(
        &self,
        boundary: &BoundaryContext,
        channel_instance_id: &str,
        _selectors: &BTreeMap<String, String>,
    ) -> Result<Option<SessionId>, BoundaryError> {
        Err(BoundaryError {
            code: BoundaryErrorCode::InvalidScope,
            operation: "resolve_external_session".into(),
            request_id: boundary.request_id.clone(),
            message: format!("runtime session resolution is unavailable for {channel_instance_id}"),
            retry_after_ms: None,
        })
    }
    /// Read one adapter-owned routing value for an emitted Session event.
    ///
    /// Runtime verifies that the Session belongs to the bound Channel
    /// instance before returning the value. The default returns no value.
    async fn session_external_value(
        &self,
        _channel_instance_id: &str,
        _session_id: &SessionId,
        _key: &str,
    ) -> Result<Option<String>, BoundaryError> {
        Ok(None)
    }
    /// Return Agent definitions visible to the authenticated principal.
    async fn discover_agents(
        &self,
        boundary: &BoundaryContext,
    ) -> Result<Vec<AgentDescriptor>, BoundaryError>;
    /// Create and persist one authorized session.
    async fn create_session(
        &self,
        boundary: &BoundaryContext,
        request: SessionCreateRequest,
    ) -> Result<SessionConfigState, BoundaryError>;
    /// Read the effective configuration of one visible session.
    async fn session_config(
        &self,
        boundary: &BoundaryContext,
        session_id: &SessionId,
    ) -> Result<SessionConfigState, BoundaryError>;
    /// Validate and apply a typed configuration update.
    async fn update_session_config(
        &self,
        boundary: &BoundaryContext,
        request: SessionConfigUpdateRequest,
    ) -> Result<SessionConfigState, BoundaryError>;
    /// Persist bounded run feedback and return its stable identifier.
    async fn submit_feedback(
        &self,
        boundary: &BoundaryContext,
        feedback: RunFeedback,
    ) -> Result<String, BoundaryError>;

    /// Advertise identity-binding versions installed by this Runtime.
    ///
    /// The default is empty and therefore denies every identity operation.
    fn identity_binding_capabilities(&self) -> IdentityBindingCapabilities {
        IdentityBindingCapabilities::default()
    }

    /// Apply one identity operation to the ingress-derived transport identity.
    ///
    /// Channels receive no identity store and no constructor for `identity`.
    /// A Runtime override must authorize `boundary` again before consuming the
    /// identity parts. The default response is content-safe and fail-closed.
    async fn identity_binding(
        &self,
        _boundary: &BoundaryContext,
        _identity: AuthenticatedTransportIdentity,
        request: IdentityBindingRequest,
    ) -> IdentityBindingResponse {
        identity_error_response(
            request.operation(),
            IdentityBindingErrorCode::ServiceUnavailable,
            "identity binding service is unavailable",
        )
    }

    /// Advertise User Profile versions installed by this Runtime.
    ///
    /// The default is empty and therefore denies every profile operation.
    fn user_profile_capabilities(&self) -> UserProfileCapabilities {
        UserProfileCapabilities::default()
    }

    /// Apply one profile operation to the boundary-derived stable user.
    ///
    /// The request has no owner selector. A Runtime override must authenticate
    /// and resolve `boundary` to a stable user before store access. The default
    /// never reflects profile content and fails closed.
    async fn user_profile(
        &self,
        _boundary: &BoundaryContext,
        request: UserProfileRequest,
    ) -> UserProfileResponse {
        UserProfileResponse::Error {
            version: USER_PROFILE_PROTOCOL_VERSION,
            error: UserProfileError::service_unavailable(request.operation()),
        }
    }

    /// List or resolve owner- and session-bound Guardian confirmations.
    ///
    /// Requests carry no owner selector. Runtime must derive the stable user
    /// from `boundary`, verify session ownership, and compare the candidate's
    /// persisted origin session before returning or applying a decision.
    async fn memory_confirmation(
        &self,
        _boundary: &BoundaryContext,
        request: MemoryConfirmationRequest,
    ) -> MemoryConfirmationResponse {
        MemoryConfirmationResponse::service_unavailable(request.operation())
    }

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

    /// Return a session-scoped context report after verifying ownership.
    async fn context_report(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_api::ContextReport, BoundaryError> {
        Err(unavailable_ui_control(boundary, "context_report"))
    }

    /// Compact an idle session after verifying ownership.
    async fn compact_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_api::CompactionReport, BoundaryError> {
        Err(unavailable_ui_control(boundary, "compact_session"))
    }

    /// Preview the latest workspace rollback after verifying ownership.
    async fn preview_workspace_rollback(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_api::WorkspaceRollbackPreview, BoundaryError> {
        Err(unavailable_ui_control(
            boundary,
            "preview_workspace_rollback",
        ))
    }

    /// Apply the latest workspace rollback after verifying ownership.
    async fn rollback_workspace(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
        _expected_turn_id: &str,
    ) -> Result<sylvander_api::WorkspaceRollbackReport, BoundaryError> {
        Err(unavailable_ui_control(boundary, "rollback_workspace"))
    }

    /// Inspect the isolated coding worktree owned by a session.
    async fn inspect_coding_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<sylvander_api::CodingSessionDiff, BoundaryError> {
        Err(unavailable_ui_control(boundary, "inspect_coding_session"))
    }

    /// Merge the reviewed coding worktree into its source branch.
    async fn accept_coding_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<(), BoundaryError> {
        Err(unavailable_ui_control(boundary, "accept_coding_session"))
    }

    /// Delete an isolated coding worktree and its session.
    async fn discard_coding_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<(), BoundaryError> {
        Err(unavailable_ui_control(boundary, "discard_coding_session"))
    }

    /// Permanently close a session through the Runtime lifecycle boundary.
    ///
    /// Transports must not delete the shared session store directly because
    /// Agent detachment, worktree cleanup, and Guardian curation are one
    /// Runtime-owned lifecycle.
    async fn delete_session(
        &self,
        boundary: &BoundaryContext,
        _session_id: &SessionId,
    ) -> Result<(), BoundaryError> {
        Err(unavailable_ui_control(boundary, "delete_session"))
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

fn identity_error_response(
    operation: IdentityBindingOperation,
    code: IdentityBindingErrorCode,
    message: &'static str,
) -> IdentityBindingResponse {
    IdentityBindingResponse::Error {
        version: IDENTITY_BINDING_PROTOCOL_VERSION,
        error: IdentityBindingError {
            code,
            operation,
            message: message.into(),
            retry_after_ms: None,
        },
    }
}

fn unavailable_ui_control(boundary: &BoundaryContext, operation: &str) -> BoundaryError {
    BoundaryError {
        code: BoundaryErrorCode::InvalidScope,
        operation: operation.into(),
        request_id: boundary.request_id.clone(),
        message: "authenticated runtime control is unavailable".into(),
        retry_after_ms: None,
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
/// 3. The channel starts its event loop, submitting mutations exclusively
///    through the runtime-owned [`ChannelHost`]
/// 4. On shutdown, the runtime requests a cooperative drain and waits for the
///    channel to stop
///
/// # Contract
///
/// - The channel MUST NOT call engine or agent methods directly
/// - Mutations and controls flow through [`ChannelHost`]
/// - Session lookup and mutation go through [`ChannelHost`]; channels never
///   receive the Runtime's persistence backend
#[async_trait]
pub trait Channel: Send + Sync {
    /// Human-readable channel name (for logging).
    fn name(&self) -> &str;

    /// Start the channel's event loop.
    ///
    /// The channel should:
    /// - Listen for external messages (stdin, webhook, polling, ...)
    /// - Subscribe to the bus for agent events
    /// - Ask [`ChannelHost`] to resolve or create Runtime sessions
    /// - Submit normalized messages through [`ChannelContext::submit_control`]
    ///   or [`submit_external_chat`]
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
    /// Runtime-owned UI application service. Channels adapt transports only.
    pub host: Option<Arc<dyn ChannelHost>>,
    /// Runtime-owned startup handshake. Channel implementations call
    /// [`ChannelContext::mark_ready`] only after external input can arrive.
    #[doc(hidden)]
    pub readiness: Option<ChannelReadiness>,
    channel_instance_id: Option<String>,
    session_defaults: SessionConfigOverrides,
}

impl ChannelContext {
    /// Construct a test or explicitly ephemeral context without Runtime UI services.
    #[must_use]
    pub fn new(bus: Arc<dyn MessageBus>) -> Self {
        Self {
            bus,
            host: None,
            readiness: None,
            channel_instance_id: None,
            session_defaults: SessionConfigOverrides::default(),
        }
    }

    /// Construct the production context supplied to a supervised channel.
    #[must_use]
    pub fn with_runtime_services(
        bus: Arc<dyn MessageBus>,
        channel_instance_id: impl Into<String>,
        host: Arc<dyn ChannelHost>,
        readiness: Option<ChannelReadiness>,
    ) -> Self {
        Self::with_services(bus, Some(channel_instance_id.into()), Some(host), readiness)
    }

    #[doc(hidden)]
    #[must_use]
    pub fn with_runtime_services_and_defaults(
        bus: Arc<dyn MessageBus>,
        channel_instance_id: impl Into<String>,
        host: Arc<dyn ChannelHost>,
        readiness: Option<ChannelReadiness>,
        session_defaults: SessionConfigOverrides,
    ) -> Self {
        Self {
            bus,
            host: Some(host),
            readiness,
            channel_instance_id: Some(channel_instance_id.into()),
            session_defaults,
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn with_services(
        bus: Arc<dyn MessageBus>,
        channel_instance_id: Option<String>,
        host: Option<Arc<dyn ChannelHost>>,
        readiness: Option<ChannelReadiness>,
    ) -> Self {
        Self {
            bus,
            host,
            readiness,
            channel_instance_id,
            session_defaults: SessionConfigOverrides::default(),
        }
    }

    /// Resolve adapter-native identifiers through the scoped Runtime host.
    pub async fn resolve_external_session(
        &self,
        boundary: &BoundaryContext,
        selectors: &BTreeMap<String, String>,
    ) -> Result<Option<SessionId>, BoundaryError> {
        let host = self.host.as_ref().ok_or_else(|| BoundaryError {
            code: BoundaryErrorCode::InvalidScope,
            operation: "resolve_external_session".into(),
            request_id: boundary.request_id.clone(),
            message: "runtime authorization service is unavailable".into(),
            retry_after_ms: None,
        })?;
        let instance_id = self
            .channel_instance_id
            .as_deref()
            .ok_or_else(|| BoundaryError::forbidden(boundary, "resolve_external_session"))?;
        if boundary.channel_instance_id != instance_id {
            return Err(BoundaryError::forbidden(
                boundary,
                "resolve_external_session",
            ));
        }
        host.resolve_external_session(boundary, instance_id, selectors)
            .await
    }

    /// Read one scoped routing value for a Runtime-emitted Session event.
    pub async fn session_external_value(
        &self,
        session_id: &SessionId,
        key: &str,
    ) -> Result<Option<String>, BoundaryError> {
        let host = self.host.as_ref().ok_or_else(|| BoundaryError {
            code: BoundaryErrorCode::InvalidScope,
            operation: "session_external_value".into(),
            request_id: String::new(),
            message: "runtime authorization service is unavailable".into(),
            retry_after_ms: None,
        })?;
        let instance_id = self
            .channel_instance_id
            .as_deref()
            .ok_or_else(|| BoundaryError {
                code: BoundaryErrorCode::InvalidScope,
                operation: "session_external_value".into(),
                request_id: String::new(),
                message: "channel instance scope is unavailable".into(),
                retry_after_ms: None,
            })?;
        host.session_external_value(instance_id, session_id, key)
            .await
    }

    /// Subscribe to runtime output without obtaining publish authority.
    pub async fn subscribe(
        &self,
        filter: SubscriptionFilter,
    ) -> Result<tokio::sync::mpsc::Receiver<BusMessage>, BusError> {
        self.bus.subscribe(filter).await
    }

    /// Submit one authenticated interactive control through the UI boundary.
    pub async fn submit_control(
        &self,
        boundary: &BoundaryContext,
        message: UiClientMessage,
    ) -> Result<(), BoundaryError> {
        let host = self.host.as_ref().ok_or_else(|| BoundaryError {
            code: BoundaryErrorCode::InvalidScope,
            operation: "submit_control".into(),
            request_id: boundary.request_id.clone(),
            message: "runtime authorization service is unavailable".into(),
            retry_after_ms: None,
        })?;
        host.submit_control(boundary, message).await
    }

    /// Return identity versions installed by the Runtime.
    ///
    /// An absent service and the trait default both advertise no capability.
    #[must_use]
    pub fn identity_binding_capabilities(&self) -> IdentityBindingCapabilities {
        self.host
            .as_ref()
            .map_or_else(IdentityBindingCapabilities::default, |host| {
                host.identity_binding_capabilities()
            })
    }

    /// Submit an identity action for the principal authenticated by this
    /// concrete Channel ingress.
    ///
    /// This method derives the transport identity; the serializable request
    /// cannot override any identity component. Adapters must never deserialize
    /// a caller-provided `BoundaryContext` in place of their authentication
    /// result.
    pub async fn submit_identity_binding(
        &self,
        boundary: &BoundaryContext,
        request: IdentityBindingRequest,
    ) -> IdentityBindingResponse {
        let operation = request.operation();
        let identity = match AuthenticatedTransportIdentity::from_ingress(boundary) {
            Ok(identity) => identity,
            Err(IdentityIngressError::Unauthenticated) => {
                return identity_error_response(
                    operation,
                    IdentityBindingErrorCode::Unauthenticated,
                    "authentication is required",
                );
            }
            Err(IdentityIngressError::Forbidden | IdentityIngressError::Invalid) => {
                return identity_error_response(
                    operation,
                    IdentityBindingErrorCode::Forbidden,
                    "the authenticated principal cannot bind an identity",
                );
            }
        };

        if let Err(error) = request.validate() {
            let (code, message) = match error {
                IdentityBindingValidationError::UnsupportedVersion => (
                    IdentityBindingErrorCode::UnsupportedVersion,
                    "identity binding protocol version is unsupported",
                ),
                IdentityBindingValidationError::InvalidChallengeId
                | IdentityBindingValidationError::InvalidSecret => (
                    IdentityBindingErrorCode::InvalidRequest,
                    "identity binding request is invalid",
                ),
            };
            return identity_error_response(operation, code, message);
        }

        let Some(host) = self.host.as_ref() else {
            return identity_error_response(
                operation,
                IdentityBindingErrorCode::ServiceUnavailable,
                "identity binding service is unavailable",
            );
        };
        if !host
            .identity_binding_capabilities()
            .supports(request.version)
        {
            return identity_error_response(
                operation,
                IdentityBindingErrorCode::ServiceUnavailable,
                "identity binding service is unavailable",
            );
        }

        host.identity_binding(boundary, identity, request).await
    }

    /// Notify Runtime that the adapter can accept external input.
    pub fn mark_ready(&self) {
        if let Some(readiness) = &self.readiness {
            readiness.mark_ready();
        }
    }

    /// Wait until Runtime asks this channel attempt to drain and stop.
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
    mut request: ExternalChatRequest,
) -> Result<SubmittedChat, BoundaryError> {
    let host = context.host.as_ref().ok_or_else(|| BoundaryError {
        code: BoundaryErrorCode::InvalidScope,
        operation: "external_chat".into(),
        request_id: boundary.request_id.clone(),
        message: "runtime authorization service is unavailable".into(),
        retry_after_ms: None,
    })?;

    if request.existing_session.is_none() {
        inherit_session_defaults(&mut request.overrides, &context.session_defaults);
    }
    host.submit_chat(boundary, request).await
}

/// Parse a small transport-neutral command surface for chat-only adapters.
///
/// Unknown slash commands remain ordinary chat. Recognized controls require
/// an existing owned session and are still authorized by `ChannelHost`.
pub fn parse_external_control(
    text: &str,
    session_id: Option<&SessionId>,
) -> Option<Result<UiClientMessage, &'static str>> {
    let text = text.trim();
    let command = text.split_whitespace().next()?;
    if !matches!(command, "/approve" | "/deny" | "/answer" | "/interrupt") {
        return None;
    }
    let Some(session_id) = session_id else {
        return Some(Err("no active session for this control"));
    };
    let session_id = session_id.0.clone();
    match command {
        "/approve" => {
            let mut parts = text.split_whitespace();
            let _ = parts.next();
            let Some(call_id) = parts.next().filter(|value| !value.is_empty()) else {
                return Some(Err(
                    "usage: /approve <request-id> [once|session|persistent]",
                ));
            };
            let scope = match parts.next().unwrap_or("once") {
                "once" => sylvander_api::ApprovalScope::Once,
                "session" => sylvander_api::ApprovalScope::Session,
                "persistent" => sylvander_api::ApprovalScope::Persistent,
                _ => {
                    return Some(Err("approval scope must be once, session, or persistent"));
                }
            };
            Some(Ok(UiClientMessage::Approve {
                session_id,
                call_id: call_id.into(),
                approved: true,
                scope,
                reason: None,
            }))
        }
        "/deny" => {
            let mut parts = text.splitn(3, char::is_whitespace);
            let _ = parts.next();
            let Some(call_id) = parts.next().filter(|value| !value.trim().is_empty()) else {
                return Some(Err("usage: /deny <request-id> [reason]"));
            };
            Some(Ok(UiClientMessage::Approve {
                session_id,
                call_id: call_id.trim().into(),
                approved: false,
                scope: sylvander_api::ApprovalScope::Once,
                reason: parts
                    .next()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            }))
        }
        "/answer" => {
            let mut parts = text.splitn(3, char::is_whitespace);
            let _ = parts.next();
            let Some(call_id) = parts.next().filter(|value| !value.trim().is_empty()) else {
                return Some(Err("usage: /answer <request-id> <answer>"));
            };
            let Some(answer) = parts
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return Some(Err("usage: /answer <request-id> <answer>"));
            };
            Some(Ok(UiClientMessage::Answer {
                session_id,
                call_id: call_id.trim().into(),
                answer: answer.into(),
            }))
        }
        "/interrupt" => Some(Ok(UiClientMessage::Interrupt { session_id })),
        _ => None,
    }
}

fn inherit_session_defaults(
    overrides: &mut SessionConfigOverrides,
    defaults: &SessionConfigOverrides,
) {
    if overrides.model.is_none() {
        overrides.model.clone_from(&defaults.model);
    }
    if overrides.reasoning_effort.is_none() {
        overrides.reasoning_effort = defaults.reasoning_effort;
    }
    if overrides.permissions.is_none() {
        overrides.permissions.clone_from(&defaults.permissions);
    }
    if overrides.prompt_profile.is_none() {
        overrides
            .prompt_profile
            .clone_from(&defaults.prompt_profile);
    }
    if overrides.system_prompt.is_none() {
        overrides.system_prompt.clone_from(&defaults.system_prompt);
    }
    if overrides.user_workspace.is_none() {
        overrides
            .user_workspace
            .clone_from(&defaults.user_workspace);
    }
    if overrides.execution_target.is_none() {
        overrides
            .execution_target
            .clone_from(&defaults.execution_target);
    }
}

/// Shared lifecycle gate between Runtime supervision and one channel instance.
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
    /// Create a not-ready lifecycle with an open shutdown signal.
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

    /// Mark this channel attempt ready exactly once.
    pub fn mark_ready(&self) {
        if !self.inner.ready.swap(true, Ordering::SeqCst) {
            self.inner.notify.notify_waiters();
        }
    }

    #[must_use]
    /// Return whether this attempt has reported readiness.
    pub fn is_ready(&self) -> bool {
        self.inner.ready.load(Ordering::SeqCst)
    }

    /// Wait until this attempt reports readiness.
    pub async fn wait(&self) {
        loop {
            // Register before observing `ready`; otherwise a mark between the
            // load and `notified().await` can be lost and block startup
            // forever.
            let notified = self.inner.notify.notified();
            if self.inner.ready.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    /// Broadcast a shutdown request to this instance and all restart attempts.
    pub fn request_shutdown(&self) {
        let _ = self.inner.shutdown.send(true);
    }

    /// Create a fresh readiness gate sharing this lifecycle's shutdown signal.
    ///
    /// A supervised restart must independently report readiness, while one
    /// shutdown request still drains every attempt of the channel instance.
    #[must_use]
    pub fn next_attempt(&self) -> Self {
        Self {
            inner: Arc::new(ReadinessInner {
                ready: AtomicBool::new(false),
                notify: tokio::sync::Notify::new(),
                shutdown: self.inner.shutdown.clone(),
            }),
        }
    }

    #[must_use]
    /// Return whether Runtime has requested shutdown.
    pub fn is_shutdown_requested(&self) -> bool {
        *self.inner.shutdown.borrow()
    }

    /// Wait until Runtime requests shutdown.
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
#[path = "../tests/unit/lib.rs"]
mod tests;
