//! Session context — per-agent, per-session conversation state.
//!
//! Each [`SessionContext`] holds the full message history for one agent
//! in one session. Multiple agents in the same session each have their
//! own isolated history — tool calls from agent A never pollute agent
//! B's conversation.
//!
//! Session persistence (JSONL / SQLite) is deferred to a later phase.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use sylvander_llm_anthropic::api::types::{Message, MessageParam};

use crate::spec::SessionId;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Current Unix timestamp in seconds.
pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ---------------------------------------------------------------------------
// SessionMetadata
// ---------------------------------------------------------------------------

/// Static metadata shared by all agents in a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    /// Working directory for this session.
    pub workspace: PathBuf,
    /// Human-readable session name.
    pub name: String,
    /// ID of the user who owns this session.
    pub user_id: String,
}

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
mod tests {
    use super::*;
    use sylvander_llm_anthropic::api::types::MessageParam;

    fn test_metadata() -> SessionMetadata {
        SessionMetadata {
            workspace: PathBuf::from("/tmp/sylvander-test"),
            name: "test-session".into(),
            user_id: "user-1".into(),
        }
    }

    #[test]
    fn new_session_context_is_empty() {
        let ctx = SessionContext::new(SessionId::new("s1"), test_metadata());
        assert_eq!(ctx.session_id, SessionId::new("s1"));
        assert!(ctx.is_empty());
        assert_eq!(ctx.len(), 0);
        assert_eq!(ctx.metadata.name, "test-session");
        assert!(ctx.created_at > 0);
        assert_eq!(ctx.created_at, ctx.updated_at);
    }

    #[test]
    fn append_user_message_grows_history() {
        let mut ctx = SessionContext::new(SessionId::new("s1"), test_metadata());
        ctx.append_user_message(MessageParam::user("Hello"));
        assert_eq!(ctx.len(), 1);
        assert!(ctx.updated_at >= ctx.created_at);
    }

    #[test]
    fn append_assistant_message_converts_to_param() {
        let mut ctx = SessionContext::new(SessionId::new("s1"), test_metadata());

        use sylvander_llm_anthropic::api::types::{
            block::TextBlockKind, ContentBlock, Message, MessageKind as ApiMessageKind,
            MessageRole, StopReason, TextBlock, Usage,
        };
        let msg = Message {
            id: "msg_1".into(),
            kind: ApiMessageKind::Message,
            role: MessageRole::Assistant,
            content: vec![ContentBlock::Text(TextBlock {
                kind: TextBlockKind::Text,
                text: "Hi there!".into(),
                cache_control: None,
            })],
            model: "test-model".into(),
            stop_reason: Some(StopReason::EndTurn),
            stop_sequence: None,
            usage: Usage {
                input_tokens: 5,
                output_tokens: 3,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };

        let len_before = ctx.len();
        ctx.append_assistant_message(msg);
        assert_eq!(ctx.len(), len_before + 1);
        assert!(ctx.updated_at >= ctx.created_at);
    }

    #[test]
    fn history_snapshot_is_independent() {
        let mut ctx = SessionContext::new(SessionId::new("s1"), test_metadata());
        ctx.append_user_message(MessageParam::user("first"));

        let snap = ctx.history_snapshot();
        assert_eq!(snap.len(), 1);

        // Mutate original — snapshot unchanged
        ctx.append_user_message(MessageParam::user("second"));
        assert_eq!(snap.len(), 1);
        assert_eq!(ctx.len(), 2);
    }

    #[test]
    fn multiple_sessions_have_independent_histories() {
        let mut ctx_a = SessionContext::new(SessionId::new("sa"), test_metadata());
        let mut ctx_b = SessionContext::new(SessionId::new("sb"), test_metadata());

        ctx_a.append_user_message(MessageParam::user("to A"));
        ctx_b.append_user_message(MessageParam::user("to B"));

        assert_eq!(ctx_a.len(), 1);
        assert_eq!(ctx_b.len(), 1);
    }
}
