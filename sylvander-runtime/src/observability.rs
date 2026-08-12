//! Mandatory, content-safe Runtime lifecycle observation.
//!
//! This module is deliberately closed: Runtime code emits typed facts into a
//! built-in recorder, and no plugin can replace or suppress it. Facts contain
//! trusted correlation identifiers and lifecycle state only; prompts, tool
//! inputs, model output, credentials, and user content have no field here.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::agent_definition::{AgentId, SessionId};
use sylvander_api::MessageId;

/// Content-safe classification for a failed Runtime turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeFailureKind {
    /// The addressed Session does not exist in Runtime state.
    UnknownSession,
    /// Trusted Session or caller identity could not be established.
    Authentication,
    /// The bounded Agent kernel terminated with an execution error.
    AgentLoop,
    /// Runtime could not compose a valid immutable turn.
    Configuration,
    /// A required durable Session operation failed.
    Persistence,
}

/// Durable operation represented by a persistence lifecycle fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimePersistenceOperation {
    /// Read durable Session identity before accepting work.
    InspectSession,
    /// Create the durable Session record.
    CreateSession,
    /// Restore the active conversation history.
    RestoreHistory,
    /// Atomically persist the user message and immutable turn snapshot.
    BeginTurn,
    /// Accumulate one provider iteration's usage.
    RecordUsage,
    /// Persist the terminal assistant message.
    AppendAssistant,
    /// Commit a compacted active history.
    ReplaceHistory,
}

/// Content-safe fact consumed by the built-in recorder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeEvent {
    /// Runtime authorized a channel request and formed its immutable bus
    /// envelope, but has not yet dispatched it.
    ChatAdmitted {
        request_id: String,
        session_id: SessionId,
        message_id: MessageId,
        agent_id: AgentId,
    },
    /// The admitted envelope was accepted by the Runtime-owned message bus.
    ChatDispatched {
        request_id: String,
        session_id: SessionId,
    },
    TurnStarted {
        request_id: String,
        trace_id: String,
        turn_id: String,
        session_id: SessionId,
        agent_id: AgentId,
    },
    ModelRetried {
        turn_id: String,
        session_id: SessionId,
        attempt: u32,
    },
    ToolStarted {
        turn_id: String,
        session_id: SessionId,
        tool_call_id: String,
        tool_name: String,
    },
    ToolFinished {
        turn_id: String,
        session_id: SessionId,
        tool_call_id: String,
        tool_name: String,
        succeeded: bool,
    },
    PersistenceFinished {
        turn_id: String,
        session_id: SessionId,
        operation: RuntimePersistenceOperation,
        succeeded: bool,
    },
    TurnCompleted {
        turn_id: String,
        session_id: SessionId,
    },
    TurnInterrupted {
        turn_id: String,
        session_id: SessionId,
    },
    TurnFailed {
        turn_id: String,
        session_id: SessionId,
        kind: RuntimeFailureKind,
    },
}

impl RuntimeEvent {
    pub(crate) fn chat_admitted(
        request_id: String,
        session_id: SessionId,
        message_id: MessageId,
        agent_id: AgentId,
    ) -> Self {
        Self::ChatAdmitted {
            request_id,
            session_id,
            message_id,
            agent_id,
        }
    }

    pub(crate) fn chat_dispatched(request_id: String, session_id: SessionId) -> Self {
        Self::ChatDispatched {
            request_id,
            session_id,
        }
    }
}

/// Stable, content-safe counters exposed through Runtime health reporting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeObservabilitySnapshot {
    /// Total typed facts consumed since this Runtime started.
    pub event_count: u64,
    /// Authorized channel chat requests admitted for dispatch.
    pub chat_admitted: u64,
    /// Admitted chat requests accepted by the message bus.
    pub chat_dispatched: u64,
    /// Agent turns that entered Runtime execution.
    pub turns_started: u64,
    /// Turns that durably finished successfully.
    pub turns_completed: u64,
    /// Turns explicitly interrupted by their caller.
    pub turns_interrupted: u64,
    /// Turns that reached a typed failure terminal.
    pub turns_failed: u64,
    /// Bounded provider retries attempted within turns.
    pub model_retries: u64,
    /// Tool calls presented for execution, including hidden control tools.
    pub tools_started: u64,
    /// Tool calls returning a model-visible success.
    pub tools_succeeded: u64,
    /// Rejected, timed-out, fatal, or model-visible failed tool calls.
    pub tools_failed: u64,
    /// Required Session persistence operations that committed.
    pub persistence_succeeded: u64,
    /// Required Session persistence operations that failed.
    pub persistence_failed: u64,
}

#[derive(Default)]
struct RuntimeObservabilityInner {
    event_count: AtomicU64,
    chat_admitted: AtomicU64,
    chat_dispatched: AtomicU64,
    turns_started: AtomicU64,
    turns_completed: AtomicU64,
    turns_interrupted: AtomicU64,
    turns_failed: AtomicU64,
    model_retries: AtomicU64,
    tools_started: AtomicU64,
    tools_succeeded: AtomicU64,
    tools_failed: AtomicU64,
    persistence_succeeded: AtomicU64,
    persistence_failed: AtomicU64,
}

/// Cloneable handle to the mandatory built-in Runtime recorder.
#[derive(Clone, Default)]
pub(crate) struct RuntimeObservability {
    inner: Arc<RuntimeObservabilityInner>,
}

impl RuntimeObservability {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Consume one typed fact synchronously before the caller advances its
    /// externally visible lifecycle state.
    pub(crate) fn record(&self, event: RuntimeEvent) {
        self.inner.event_count.fetch_add(1, Ordering::Relaxed);
        match event {
            RuntimeEvent::ChatAdmitted {
                request_id,
                session_id,
                message_id,
                agent_id,
            } => {
                self.inner.chat_admitted.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = "chat_admitted",
                    %request_id,
                    %session_id,
                    message_id = ?message_id,
                    %agent_id,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::ChatDispatched {
                request_id,
                session_id,
            } => {
                self.inner.chat_dispatched.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = "chat_dispatched",
                    %request_id,
                    %session_id,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::TurnStarted {
                request_id,
                trace_id,
                turn_id,
                session_id,
                agent_id,
            } => {
                self.inner.turns_started.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = "turn_started",
                    %request_id,
                    %trace_id,
                    %turn_id,
                    %session_id,
                    %agent_id,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::ModelRetried {
                turn_id,
                session_id,
                attempt,
            } => {
                self.inner.model_retries.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = "model_retried",
                    %turn_id,
                    %session_id,
                    attempt,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::ToolStarted {
                turn_id,
                session_id,
                tool_call_id,
                tool_name,
            } => {
                self.inner.tools_started.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = "tool_started",
                    %turn_id,
                    %session_id,
                    %tool_call_id,
                    %tool_name,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::ToolFinished {
                turn_id,
                session_id,
                tool_call_id,
                tool_name,
                succeeded,
            } => {
                if succeeded {
                    self.inner.tools_succeeded.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.inner.tools_failed.fetch_add(1, Ordering::Relaxed);
                }
                tracing::info!(
                    event = "tool_finished",
                    %turn_id,
                    %session_id,
                    %tool_call_id,
                    %tool_name,
                    succeeded,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::PersistenceFinished {
                turn_id,
                session_id,
                operation,
                succeeded,
            } => {
                if succeeded {
                    self.inner
                        .persistence_succeeded
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    self.inner
                        .persistence_failed
                        .fetch_add(1, Ordering::Relaxed);
                }
                tracing::info!(
                    event = "persistence_finished",
                    %turn_id,
                    %session_id,
                    operation = ?operation,
                    succeeded,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::TurnCompleted {
                turn_id,
                session_id,
            } => {
                self.inner.turns_completed.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = "turn_completed",
                    %turn_id,
                    %session_id,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::TurnInterrupted {
                turn_id,
                session_id,
            } => {
                self.inner.turns_interrupted.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = "turn_interrupted",
                    %turn_id,
                    %session_id,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::TurnFailed {
                turn_id,
                session_id,
                kind,
            } => {
                self.inner.turns_failed.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = "turn_failed",
                    %turn_id,
                    %session_id,
                    kind = ?kind,
                    "runtime lifecycle fact"
                );
            }
        }
    }

    pub(crate) fn snapshot(&self) -> RuntimeObservabilitySnapshot {
        RuntimeObservabilitySnapshot {
            event_count: self.inner.event_count.load(Ordering::Relaxed),
            chat_admitted: self.inner.chat_admitted.load(Ordering::Relaxed),
            chat_dispatched: self.inner.chat_dispatched.load(Ordering::Relaxed),
            turns_started: self.inner.turns_started.load(Ordering::Relaxed),
            turns_completed: self.inner.turns_completed.load(Ordering::Relaxed),
            turns_interrupted: self.inner.turns_interrupted.load(Ordering::Relaxed),
            turns_failed: self.inner.turns_failed.load(Ordering::Relaxed),
            model_retries: self.inner.model_retries.load(Ordering::Relaxed),
            tools_started: self.inner.tools_started.load(Ordering::Relaxed),
            tools_succeeded: self.inner.tools_succeeded.load(Ordering::Relaxed),
            tools_failed: self.inner.tools_failed.load(Ordering::Relaxed),
            persistence_succeeded: self.inner.persistence_succeeded.load(Ordering::Relaxed),
            persistence_failed: self.inner.persistence_failed.load(Ordering::Relaxed),
        }
    }
}
