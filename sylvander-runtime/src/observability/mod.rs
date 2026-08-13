//! Mandatory, content-safe Runtime lifecycle observation.
//!
//! This module is deliberately closed: Runtime code emits typed facts into a
//! built-in recorder, and no plugin can replace or suppress it. Facts contain
//! trusted correlation identifiers and lifecycle state only; prompts, tool
//! inputs, model output, credentials, and user content have no field here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use sylvander_agent::turn::machine::TurnTransition;
use sylvander_api::MessageId;

use crate::agent_definition::{AgentId, SessionId};

/// Inclusive upper bounds for the first seven duration buckets. The eighth
/// bucket contains observations above 30 seconds.
pub const RUNTIME_DURATION_BUCKET_UPPER_BOUNDS_MICROS: [u64; 7] = [
    10_000, 50_000, 100_000, 500_000, 1_000_000, 5_000_000, 30_000_000,
];

/// Bounded fixed-bucket duration distribution for one Runtime lifecycle stage.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RuntimeDurationHistogramSnapshot {
    /// Number of completed, correctly paired observations.
    pub count: u64,
    /// Saturating sum of all observed durations.
    pub total_micros: u64,
    /// Largest observed duration.
    pub max_micros: u64,
    /// Non-overlapping counts for the seven exported upper bounds plus an
    /// overflow bucket. Each bounded bucket excludes the preceding bound and
    /// includes its own upper bound. Bucket meanings are fixed by
    /// [`RUNTIME_DURATION_BUCKET_UPPER_BOUNDS_MICROS`].
    pub bucket_counts: [u64; 8],
}

impl RuntimeDurationHistogramSnapshot {
    fn observe(&mut self, duration_micros: u64) {
        self.count = self.count.saturating_add(1);
        self.total_micros = self.total_micros.saturating_add(duration_micros);
        self.max_micros = self.max_micros.max(duration_micros);
        let bucket = RUNTIME_DURATION_BUCKET_UPPER_BOUNDS_MICROS
            .iter()
            .position(|bound| duration_micros <= *bound)
            .unwrap_or(RUNTIME_DURATION_BUCKET_UPPER_BOUNDS_MICROS.len());
        self.bucket_counts[bucket] = self.bucket_counts[bucket].saturating_add(1);
    }
}

pub(crate) trait RuntimeClock: Send + Sync {
    fn now_micros(&self) -> u64;
}

struct MonotonicRuntimeClock {
    origin: Instant,
}

impl Default for MonotonicRuntimeClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl RuntimeClock for MonotonicRuntimeClock {
    fn now_micros(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

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

/// Content-safe classification for a trusted tool execution failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeToolFailureKind {
    /// The execution adapter explicitly rejected a filesystem boundary.
    FilesystemBoundaryPolicyViolation,
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
    /// Atomically persist the terminal assistant message and completed turn.
    CompleteTurn,
    /// Persist a failed or interrupted terminal.
    FinishTurn,
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
    /// The Runtime-owned message bus accepted or rejected the admitted
    /// envelope. Both outcomes close the dispatch lifecycle.
    ChatDispatchFinished {
        request_id: String,
        session_id: SessionId,
        succeeded: bool,
    },
    TurnStarted {
        request_id: String,
        trace_id: String,
        turn_id: String,
        session_id: SessionId,
        agent_id: AgentId,
    },
    TurnTransitioned {
        turn_id: String,
        session_id: SessionId,
        transition: TurnTransition,
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
        failure_kind: Option<RuntimeToolFailureKind>,
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

    pub(crate) fn chat_dispatch_finished(
        request_id: String,
        session_id: SessionId,
        succeeded: bool,
    ) -> Self {
        Self::ChatDispatchFinished {
            request_id,
            session_id,
            succeeded,
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
    /// Admitted chat requests rejected by the message bus.
    pub chat_dispatch_failed: u64,
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
    /// Explicit filesystem-boundary policy denials reported by an adapter.
    pub filesystem_policy_violations: u64,
    /// Required Session persistence operations that committed.
    pub persistence_succeeded: u64,
    /// Required Session persistence operations that failed.
    pub persistence_failed: u64,
    /// Chat envelopes admitted but not yet accepted by the message bus.
    pub active_dispatches: u64,
    /// Turns with a start fact and no terminal fact yet.
    pub active_turns: u64,
    /// Tool calls with a start fact and no terminal fact yet.
    pub active_tools: u64,
    /// Terminal facts without a matching start. A non-zero value indicates an
    /// instrumentation lifecycle defect, not a user or provider failure.
    pub unmatched_terminals: u64,
    /// Time from authorized chat admission to message-bus acceptance.
    pub dispatch_latency: RuntimeDurationHistogramSnapshot,
    /// Time from Runtime turn start to its first terminal fact.
    pub turn_latency: RuntimeDurationHistogramSnapshot,
    /// Time from tool start to its first terminal fact.
    pub tool_latency: RuntimeDurationHistogramSnapshot,
}

#[derive(Default)]
struct RuntimeTimingState {
    dispatch_started: HashMap<String, u64>,
    turn_started: HashMap<(SessionId, String), u64>,
    tool_started: HashMap<(SessionId, String, String), u64>,
    unmatched_terminals: u64,
    dispatch_latency: RuntimeDurationHistogramSnapshot,
    turn_latency: RuntimeDurationHistogramSnapshot,
    tool_latency: RuntimeDurationHistogramSnapshot,
}

struct RuntimeObservabilityInner {
    clock: Arc<dyn RuntimeClock>,
    timing: Mutex<RuntimeTimingState>,
    event_count: AtomicU64,
    chat_admitted: AtomicU64,
    chat_dispatched: AtomicU64,
    chat_dispatch_failed: AtomicU64,
    turns_started: AtomicU64,
    turns_completed: AtomicU64,
    turns_interrupted: AtomicU64,
    turns_failed: AtomicU64,
    model_retries: AtomicU64,
    tools_started: AtomicU64,
    tools_succeeded: AtomicU64,
    tools_failed: AtomicU64,
    filesystem_policy_violations: AtomicU64,
    persistence_succeeded: AtomicU64,
    persistence_failed: AtomicU64,
}

/// Cloneable handle to the mandatory built-in Runtime recorder.
#[derive(Clone)]
pub(crate) struct RuntimeObservability {
    inner: Arc<RuntimeObservabilityInner>,
}

impl RuntimeObservability {
    pub(crate) fn new() -> Self {
        Self::with_clock(Arc::new(MonotonicRuntimeClock::default()))
    }

    fn with_clock(clock: Arc<dyn RuntimeClock>) -> Self {
        Self {
            inner: Arc::new(RuntimeObservabilityInner {
                clock,
                timing: Mutex::new(RuntimeTimingState::default()),
                event_count: AtomicU64::new(0),
                chat_admitted: AtomicU64::new(0),
                chat_dispatched: AtomicU64::new(0),
                chat_dispatch_failed: AtomicU64::new(0),
                turns_started: AtomicU64::new(0),
                turns_completed: AtomicU64::new(0),
                turns_interrupted: AtomicU64::new(0),
                turns_failed: AtomicU64::new(0),
                model_retries: AtomicU64::new(0),
                tools_started: AtomicU64::new(0),
                tools_succeeded: AtomicU64::new(0),
                tools_failed: AtomicU64::new(0),
                filesystem_policy_violations: AtomicU64::new(0),
                persistence_succeeded: AtomicU64::new(0),
                persistence_failed: AtomicU64::new(0),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_clock(clock: Arc<dyn RuntimeClock>) -> Self {
        Self::with_clock(clock)
    }

    /// Consume one typed fact synchronously before the caller advances its
    /// externally visible lifecycle state.
    pub(crate) fn record(&self, event: RuntimeEvent) {
        self.record_timing(&event);
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
            RuntimeEvent::ChatDispatchFinished {
                request_id,
                session_id,
                succeeded,
            } => {
                if succeeded {
                    self.inner.chat_dispatched.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.inner
                        .chat_dispatch_failed
                        .fetch_add(1, Ordering::Relaxed);
                }
                tracing::info!(
                    event = "chat_dispatch_finished",
                    %request_id,
                    %session_id,
                    succeeded,
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
            RuntimeEvent::TurnTransitioned {
                turn_id,
                session_id,
                transition,
            } => {
                tracing::info!(
                    event = "turn_transitioned",
                    %turn_id,
                    %session_id,
                    sequence = transition.sequence,
                    iteration = transition.iteration,
                    from = transition.from.as_str(),
                    to = transition.to.as_str(),
                    reason = ?transition.reason,
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
                failure_kind,
            } => {
                if succeeded {
                    self.inner.tools_succeeded.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.inner.tools_failed.fetch_add(1, Ordering::Relaxed);
                }
                if matches!(
                    failure_kind,
                    Some(RuntimeToolFailureKind::FilesystemBoundaryPolicyViolation)
                ) {
                    self.inner
                        .filesystem_policy_violations
                        .fetch_add(1, Ordering::Relaxed);
                }
                tracing::info!(
                    event = "tool_finished",
                    %turn_id,
                    %session_id,
                    %tool_call_id,
                    %tool_name,
                    succeeded,
                    ?failure_kind,
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

    fn record_timing(&self, event: &RuntimeEvent) {
        let now = self.inner.clock.now_micros();
        let mut timing = self
            .inner
            .timing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match event {
            RuntimeEvent::ChatAdmitted { request_id, .. } => {
                timing.dispatch_started.insert(request_id.clone(), now);
            }
            RuntimeEvent::ChatDispatchFinished { request_id, .. } => {
                let started = timing.dispatch_started.remove(request_id);
                if let Some(duration) =
                    Self::elapsed_or_unmatched(started, now, &mut timing.unmatched_terminals)
                {
                    timing.dispatch_latency.observe(duration);
                }
            }
            RuntimeEvent::TurnStarted {
                turn_id,
                session_id,
                ..
            } => {
                timing
                    .turn_started
                    .insert((session_id.clone(), turn_id.clone()), now);
            }
            RuntimeEvent::ToolStarted {
                turn_id,
                session_id,
                tool_call_id,
                ..
            } => {
                timing.tool_started.insert(
                    (session_id.clone(), turn_id.clone(), tool_call_id.clone()),
                    now,
                );
            }
            RuntimeEvent::ToolFinished {
                turn_id,
                session_id,
                tool_call_id,
                ..
            } => {
                let started = timing.tool_started.remove(&(
                    session_id.clone(),
                    turn_id.clone(),
                    tool_call_id.clone(),
                ));
                if let Some(duration) =
                    Self::elapsed_or_unmatched(started, now, &mut timing.unmatched_terminals)
                {
                    timing.tool_latency.observe(duration);
                }
            }
            RuntimeEvent::TurnCompleted {
                turn_id,
                session_id,
            }
            | RuntimeEvent::TurnInterrupted {
                turn_id,
                session_id,
            }
            | RuntimeEvent::TurnFailed {
                turn_id,
                session_id,
                ..
            } => {
                let started = timing
                    .turn_started
                    .remove(&(session_id.clone(), turn_id.clone()));
                if let Some(duration) =
                    Self::elapsed_or_unmatched(started, now, &mut timing.unmatched_terminals)
                {
                    timing.turn_latency.observe(duration);
                }
            }
            RuntimeEvent::TurnTransitioned { .. }
            | RuntimeEvent::ModelRetried { .. }
            | RuntimeEvent::PersistenceFinished { .. } => {}
        }
    }

    fn elapsed_or_unmatched(
        started: Option<u64>,
        now: u64,
        unmatched_terminals: &mut u64,
    ) -> Option<u64> {
        if let Some(started) = started {
            Some(now.saturating_sub(started))
        } else {
            *unmatched_terminals = unmatched_terminals.saturating_add(1);
            None
        }
    }

    pub(crate) fn snapshot(&self) -> RuntimeObservabilitySnapshot {
        let timing = self
            .inner
            .timing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        RuntimeObservabilitySnapshot {
            event_count: self.inner.event_count.load(Ordering::Relaxed),
            chat_admitted: self.inner.chat_admitted.load(Ordering::Relaxed),
            chat_dispatched: self.inner.chat_dispatched.load(Ordering::Relaxed),
            chat_dispatch_failed: self.inner.chat_dispatch_failed.load(Ordering::Relaxed),
            turns_started: self.inner.turns_started.load(Ordering::Relaxed),
            turns_completed: self.inner.turns_completed.load(Ordering::Relaxed),
            turns_interrupted: self.inner.turns_interrupted.load(Ordering::Relaxed),
            turns_failed: self.inner.turns_failed.load(Ordering::Relaxed),
            model_retries: self.inner.model_retries.load(Ordering::Relaxed),
            tools_started: self.inner.tools_started.load(Ordering::Relaxed),
            tools_succeeded: self.inner.tools_succeeded.load(Ordering::Relaxed),
            tools_failed: self.inner.tools_failed.load(Ordering::Relaxed),
            filesystem_policy_violations: self
                .inner
                .filesystem_policy_violations
                .load(Ordering::Relaxed),
            persistence_succeeded: self.inner.persistence_succeeded.load(Ordering::Relaxed),
            persistence_failed: self.inner.persistence_failed.load(Ordering::Relaxed),
            active_dispatches: timing.dispatch_started.len() as u64,
            active_turns: timing.turn_started.len() as u64,
            active_tools: timing.tool_started.len() as u64,
            unmatched_terminals: timing.unmatched_terminals,
            dispatch_latency: timing.dispatch_latency,
            turn_latency: timing.turn_latency,
            tool_latency: timing.tool_latency,
        }
    }
}

impl Default for RuntimeObservability {
    fn default() -> Self {
        Self::new()
    }
}
