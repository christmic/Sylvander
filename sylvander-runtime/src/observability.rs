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
}

#[derive(Default)]
struct RuntimeObservabilityInner {
    event_count: AtomicU64,
    chat_admitted: AtomicU64,
    chat_dispatched: AtomicU64,
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
        }
    }

    pub(crate) fn snapshot(&self) -> RuntimeObservabilitySnapshot {
        RuntimeObservabilitySnapshot {
            event_count: self.inner.event_count.load(Ordering::Relaxed),
            chat_admitted: self.inner.chat_admitted.load(Ordering::Relaxed),
            chat_dispatched: self.inner.chat_dispatched.load(Ordering::Relaxed),
        }
    }
}
