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

use sylvander_agent::bus::MessageBus;
use sylvander_agent::session_store::SessionStore;
use sylvander_protocol::{
    AgentDescriptor, AgentId, BoundaryContext, BoundaryError, BoundaryErrorCode, RunFeedback,
    SessionConfigOverrides, SessionConfigState, SessionConfigUpdateRequest, SessionCreateRequest,
    SessionId, UiClientMessage,
};

/// Transport-neutral UI service boundary owned by the runtime.
#[async_trait]
pub trait UiService: Send + Sync {
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
    /// Publish messages to the bus, subscribe to events.
    pub bus: Arc<dyn MessageBus>,
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

/// Resolve or create a platform-owned session through the runtime boundary.
///
/// External adapters use this instead of writing a session and publishing a
/// message directly. It applies Agent access policy on creation and session
/// ownership, payload, and rate policy to every inbound chat message.
pub async fn authorize_external_chat(
    context: &ChannelContext,
    boundary: &BoundaryContext,
    existing_session: Option<SessionId>,
    agent_id: AgentId,
    label: String,
    overrides: SessionConfigOverrides,
    text: &str,
    attachments: &[sylvander_protocol::MessageAttachment],
    external_meta: BTreeMap<String, String>,
) -> Result<SessionId, BoundaryError> {
    let ui = context.ui.as_ref().ok_or_else(|| BoundaryError {
        code: BoundaryErrorCode::InvalidScope,
        operation: "external_chat".into(),
        request_id: boundary.request_id.clone(),
        message: "runtime authorization service is unavailable".into(),
        retry_after_ms: None,
    })?;

    let session_id = if let Some(session_id) = existing_session {
        session_id
    } else {
        let request = SessionCreateRequest {
            agent_id,
            label,
            channel_id: Some(boundary.channel_instance_id.clone()),
            overrides,
        };
        ui.authorize_message(
            boundary,
            &UiClientMessage::CreateSession {
                request: request.clone(),
            },
        )
        .await?;
        let state = ui.create_session(boundary, request).await?;
        let mut session = context
            .sessions
            .get(&state.session_id)
            .await
            .map_err(|error| external_session_error(boundary, error.to_string()))?
            .ok_or_else(|| external_session_error(boundary, "created session was not persisted"))?;
        for (key, value) in external_meta {
            session
                .external_meta
                .insert(key, serde_json::Value::String(value));
        }
        context
            .sessions
            .save(&session)
            .await
            .map_err(|error| external_session_error(boundary, error.to_string()))?;
        state.session_id
    };

    ui.authorize_message(
        boundary,
        &UiClientMessage::Chat {
            text: text.into(),
            attachments: attachments.to_vec(),
            session_id: Some(session_id.0.clone()),
            workspace: None,
        },
    )
    .await?;
    Ok(session_id)
}

fn external_session_error(boundary: &BoundaryContext, message: impl Into<String>) -> BoundaryError {
    BoundaryError {
        code: BoundaryErrorCode::InvalidScope,
        operation: "external_chat".into(),
        request_id: boundary.request_id.clone(),
        message: message.into(),
        retry_after_ms: None,
    }
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

        let error = authorize_external_chat(
            &context,
            &boundary,
            None,
            AgentId::new("assistant"),
            "telegram-42".into(),
            SessionConfigOverrides::default(),
            "hello",
            &[],
            BTreeMap::new(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, BoundaryErrorCode::InvalidScope);
    }
}
