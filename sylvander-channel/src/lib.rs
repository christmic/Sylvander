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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;

use sylvander_agent::bus::MessageBus;
use sylvander_agent::session_store::SessionStore;
use sylvander_protocol::{
    AgentDescriptor, SessionConfigState, SessionConfigUpdateRequest, SessionId,
};

/// Transport-neutral UI service boundary owned by the runtime.
#[async_trait]
pub trait UiService: Send + Sync {
    async fn discover_agents(&self) -> Vec<AgentDescriptor>;
    async fn session_config(&self, session_id: &SessionId) -> Result<SessionConfigState, String>;
    async fn update_session_config(
        &self,
        request: SessionConfigUpdateRequest,
    ) -> Result<SessionConfigState, String>;
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
