//! Protocol adapter trait — the interface between external channels
//! (TUI, Telegram, HTTP API) and the internal message bus.
//!
//! Adapters translate external message formats to normalized
//! [`BusMessage`](sylvander_agent::bus::BusMessage)s. Protocol metadata
//! (chat_id, thread_id, window coordinates) is extracted and stored in
//! the session — agents never see it.

use std::sync::Arc;

use async_trait::async_trait;

use sylvander_agent::bus::MessageBus;

use crate::session_store::SessionStore;

// ---------------------------------------------------------------------------
// ProtocolAdapter
// ---------------------------------------------------------------------------

/// A channel adapter — normalizes external messages for the bus.
///
/// # Responsibilities
///
/// 1. Receive messages in their native format
/// 2. Extract protocol metadata → store in session
/// 3. Map external IDs → internal [`SessionId`](sylvander_agent::spec::SessionId)
/// 4. Publish normalized `BusMessage`s
///
/// # Implementors
///
/// - `TuiAdapter` — stdin/stdout terminal UI
/// - `TelegramAdapter` — Telegram Bot API
/// - `HttpAdapter` — REST API for web clients
#[async_trait]
pub trait ProtocolAdapter: Send + Sync {
    /// Human-readable name (for logging).
    fn name(&self) -> &str;

    /// Start the adapter's event loop.
    ///
    /// Runs until shutdown (tokio cancellation or explicit stop).
    /// Communicates exclusively through the bus and session store —
    /// never by calling engine methods directly.
    async fn run(
        self: Arc<Self>,
        bus: Arc<dyn MessageBus>,
        session_store: Arc<dyn SessionStore>,
    );
}
