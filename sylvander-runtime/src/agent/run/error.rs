//! Runtime Agent-run failure taxonomy and persistence operation context.

use sylvander_agent::turn::error::AgentLoopError;
use sylvander_api::SessionId;

use crate::storage::session::SessionStoreError;

pub(super) fn prompt_integrity_error() -> AgentRunError {
    AgentRunError::Configuration("prompt integrity verification failed".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPersistenceOperation {
    InspectSession,
    CreateSession,
    RestoreHistory,
    BeginTurn,
    BeginToolCall,
    PersistModelToolResponse,
    AdvanceToolCall,
    PersistToolResult,
    FinishToolCall,
    RecordUsage,
    CompleteTurn,
    FinishTurn,
    ReplaceHistory,
}

impl std::fmt::Display for SessionPersistenceOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InspectSession => "inspect_session",
            Self::CreateSession => "create_session",
            Self::RestoreHistory => "restore_history",
            Self::BeginTurn => "begin_turn",
            Self::BeginToolCall => "begin_tool_call",
            Self::PersistModelToolResponse => "persist_model_tool_response",
            Self::AdvanceToolCall => "advance_tool_call",
            Self::PersistToolResult => "persist_tool_result",
            Self::FinishToolCall => "finish_tool_call",
            Self::RecordUsage => "record_usage",
            Self::CompleteTurn => "complete_turn",
            Self::FinishTurn => "finish_turn",
            Self::ReplaceHistory => "replace_history",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    #[error("unknown session: {0}")]
    UnknownSession(SessionId),
    #[error("session authentication error: {0}")]
    Authentication(String),
    #[error("loop error: {0}")]
    Loop(#[from] AgentLoopError),
    #[error("build error: {0}")]
    Build(String),
    #[error("session configuration error: {0}")]
    Configuration(String),
    #[error("session persistence failed during {operation}")]
    SessionPersistence {
        operation: SessionPersistenceOperation,
        #[source]
        source: SessionStoreError,
    },
}

impl AgentRunError {
    pub(super) fn session_persistence(
        operation: SessionPersistenceOperation,
        source: SessionStoreError,
    ) -> Self {
        Self::SessionPersistence { operation, source }
    }
}
