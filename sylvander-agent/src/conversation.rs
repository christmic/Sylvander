//! Model-visible conversation state for one Agent execution.
//!
//! This module distinguishes an in-memory model transcript from Runtime's
//! durable product Session. The Agent may rewrite this snapshot through
//! compression and append model/tool messages while executing, but it cannot
//! authenticate, persist, resume, or archive the originating Session.

use sylvander_llm_core::ChatMessage;

/// Immutable-at-entry snapshot of the messages visible to the model.
///
/// Runtime owns persistence and product Session revisions. The Agent owns this
/// value only while executing one turn and returns an updated snapshot in its
/// outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationSnapshot {
    messages: Vec<ChatMessage>,
}

impl ConversationSnapshot {
    /// Create a snapshot from the exact ordered messages Runtime selected.
    #[must_use]
    pub const fn new(messages: Vec<ChatMessage>) -> Self {
        Self { messages }
    }

    /// Borrow the exact ordered messages visible at this point in execution.
    #[must_use]
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Return ownership of the messages for request construction or commit.
    #[must_use]
    pub fn into_messages(self) -> Vec<ChatMessage> {
        self.messages
    }
}

#[cfg(test)]
#[path = "../tests/unit/conversation.rs"]
mod tests;
