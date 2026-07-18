//! Session context — per-agent, per-session conversation state.
//!
//! Each [`SessionContext`](crate::session::SessionContext) holds the full message history for one agent
//! in one session. Multiple agents in the same session each have their
//! own isolated history — tool calls from agent A never pollute agent
//! B's conversation.
//!
//! This value is the in-memory turn view. Durable ownership belongs to the
//! injected [`crate::session_store::SessionStore`]; production Runtime uses
//! the SQLite implementation and rebuilds this view from persisted messages.

use std::time::{SystemTime, UNIX_EPOCH};

use sylvander_llm_anthropic::api::types::{Message, MessageParam};

use crate::spec::SessionId;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Current Unix timestamp in seconds.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------------------
// SessionMetadata
// ---------------------------------------------------------------------------

pub use sylvander_protocol::SessionMetadata;

// ---------------------------------------------------------------------------
// SessionContext
// ---------------------------------------------------------------------------

/// Per-agent, per-session conversation state.
///
/// Holds the complete message history for one agent within one session.
/// The history is *isolated* — different agents in the same session
/// have independent views of the conversation.
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// Which session this context belongs to.
    pub session_id: SessionId,
    /// The full message history for this agent in this session.
    pub history: Vec<MessageParam>,
    /// Shared session metadata.
    pub metadata: SessionMetadata,
    /// When this context was first created.
    pub created_at: i64,
    /// When this context was last modified.
    pub updated_at: i64,
}

impl SessionContext {
    /// Create a new empty session context.
    #[must_use]
    pub fn new(session_id: SessionId, metadata: SessionMetadata) -> Self {
        let now = now_secs();
        Self {
            session_id,
            history: Vec::new(),
            metadata,
            created_at: now,
            updated_at: now,
        }
    }

    /// Append a user message to the history.
    pub fn append_user_message(&mut self, msg: MessageParam) {
        self.history.push(msg);
        self.updated_at = now_secs();
    }

    /// Append an assistant [`Message`] to the history.
    ///
    /// Converts the response `Message` into a re-feedable `MessageParam`
    /// so it can be passed back to the LLM on subsequent turns.
    pub fn append_assistant_message(&mut self, msg: Message) {
        self.history
            .push(MessageParam::assistant_blocks(msg.content));
        self.updated_at = now_secs();
    }

    /// Return a clone of the current history for loop input.
    ///
    /// The snapshot is independent — subsequent mutations to
    /// [`Self::history`] do not affect it.
    #[must_use]
    pub fn history_snapshot(&self) -> Vec<MessageParam> {
        self.history.clone()
    }

    /// Number of messages in the history.
    #[must_use]
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// `true` if the history is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "../tests/unit/session.rs"]
mod tests;
