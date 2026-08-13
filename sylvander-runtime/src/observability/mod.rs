//! Mandatory, content-safe Runtime lifecycle observation.
//!
//! This module is deliberately closed: Runtime code emits typed facts into a
//! built-in recorder, and no plugin can replace or suppress it. Facts contain
//! trusted correlation identifiers and lifecycle state only; prompts, tool
//! inputs, model output, credentials, and user content have no field here.

mod debug_log;

pub(crate) use debug_log::RuntimeObservationDebugLog;
#[cfg(test)]
pub(crate) use debug_log::{
    DEBUG_OBSERVATION_LOG_MAX_FILES, DEBUG_OBSERVATION_LOG_TOTAL_MAX_BYTES,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use sylvander_agent::turn::machine::TurnTransition;
use sylvander_api::MessageId;
use tokio::sync::broadcast;

use crate::agent_definition::{AgentId, SessionId};
use crate::storage::session::{
    CognitionExecutionPosition, CognitionRecoveryDecision, ModelExecutionPosition,
    ModelRecoveryDecision, PerceptionExecutionPosition, PerceptionRecoveryDecision,
    ToolExecutionPosition, ToolRecoveryDecision,
};

/// Inclusive upper bounds for the first seven duration buckets. The eighth
/// bucket contains observations above 30 seconds.
pub const RUNTIME_DURATION_BUCKET_UPPER_BOUNDS_MICROS: [u64; 7] = [
    10_000, 50_000, 100_000, 500_000, 1_000_000, 5_000_000, 30_000_000,
];

/// Per-consumer capacity of the non-blocking governance observation bus.
pub(crate) const RUNTIME_OBSERVATION_BUFFER_CAPACITY: usize = 1_024;

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

impl RuntimeFailureKind {
    const UNKNOWN_SESSION: &'static str = "unknown_session";
    const AUTHENTICATION: &'static str = "authentication";
    const AGENT_LOOP: &'static str = "agent_loop";
    const CONFIGURATION: &'static str = "configuration";
    const PERSISTENCE: &'static str = "persistence";

    const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownSession => Self::UNKNOWN_SESSION,
            Self::Authentication => Self::AUTHENTICATION,
            Self::AgentLoop => Self::AGENT_LOOP,
            Self::Configuration => Self::CONFIGURATION,
            Self::Persistence => Self::PERSISTENCE,
        }
    }
}

/// Content-safe classification for a trusted tool execution failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeToolFailureKind {
    /// The execution adapter explicitly rejected a filesystem boundary.
    FilesystemBoundaryPolicyViolation,
}

/// Low-cardinality outcome for governed multi-Agent coordination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeCoordinationOutcome {
    Enqueued,
    TaskCreated,
    TaskTransitioned,
    TaskClaimed,
    TaskLeaseRecovered,
    TaskCancelled,
    BackgroundDispatchRecovered,
    ParticipantActivated,
    TopologyUpdated,
    ArbitrationRequired,
    ModeratorAuthorized,
    ModeratorRejected,
    ArbitrationApplied,
    MailboxEscalated,
    WorkspaceReviewPrepared,
    WorkspaceApproved,
    WorkspaceApplied,
    WorkspaceMergeRecovered,
    WorkspaceConflicted,
    RecoveryAbandoned,
    RecoveryRetryAuthorized,
}

impl RuntimeCoordinationOutcome {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Enqueued => "enqueued",
            Self::TaskCreated => "task_created",
            Self::TaskTransitioned => "task_transitioned",
            Self::TaskClaimed => "task_claimed",
            Self::TaskLeaseRecovered => "task_lease_recovered",
            Self::TaskCancelled => "task_cancelled",
            Self::BackgroundDispatchRecovered => "background_dispatch_recovered",
            Self::ParticipantActivated => "participant_activated",
            Self::TopologyUpdated => "topology_updated",
            Self::ArbitrationRequired => "arbitration_required",
            Self::ModeratorAuthorized => "moderator_authorized",
            Self::ModeratorRejected => "moderator_rejected",
            Self::ArbitrationApplied => "arbitration_applied",
            Self::MailboxEscalated => "mailbox_escalated",
            Self::WorkspaceReviewPrepared => "workspace_review_prepared",
            Self::WorkspaceApproved => "workspace_approved",
            Self::WorkspaceApplied => "workspace_applied",
            Self::WorkspaceMergeRecovered => "workspace_merge_recovered",
            Self::WorkspaceConflicted => "workspace_conflicted",
            Self::RecoveryAbandoned => "recovery_abandoned",
            Self::RecoveryRetryAuthorized => "recovery_retry_authorized",
        }
    }
}

impl RuntimeToolFailureKind {
    const FILESYSTEM_BOUNDARY_POLICY_VIOLATION: &'static str =
        "filesystem_boundary_policy_violation";

    const fn as_str(self) -> &'static str {
        match self {
            Self::FilesystemBoundaryPolicyViolation => Self::FILESYSTEM_BOUNDARY_POLICY_VIOLATION,
        }
    }
}

/// Durable operation represented by a persistence lifecycle fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimePersistenceOperation {
    /// Read durable Session identity before accepting work.
    InspectSession,
    /// Create the durable Session record.
    CreateSession,
    /// Restore or initialize the Session's first-class Agent membership.
    RestoreMembership,
    /// Restore the active conversation history.
    RestoreHistory,
    /// Atomically persist the user message and immutable turn snapshot.
    BeginTurn,
    /// Persist tool identity before approval or execution.
    BeginToolCall,
    /// Persist a complete assistant message before its tool effects.
    PersistModelToolResponse,
    /// Advance a write-ahead tool effect boundary.
    AdvanceToolCall,
    /// Persist a model-visible tool observation and its ledger boundary.
    PersistToolResult,
    /// Persist the tool's unique terminal state.
    FinishToolCall,
    /// Accumulate one provider iteration's usage.
    RecordUsage,
    /// Atomically persist the terminal assistant message and completed turn.
    CompleteTurn,
    /// Persist a failed or interrupted terminal.
    FinishTurn,
    /// Commit a compacted active history.
    ReplaceHistory,
}

impl RuntimePersistenceOperation {
    const INSPECT_SESSION: &'static str = "inspect_session";
    const CREATE_SESSION: &'static str = "create_session";
    const RESTORE_MEMBERSHIP: &'static str = "restore_membership";
    const RESTORE_HISTORY: &'static str = "restore_history";
    const BEGIN_TURN: &'static str = "begin_turn";
    const BEGIN_TOOL_CALL: &'static str = "begin_tool_call";
    const PERSIST_MODEL_TOOL_RESPONSE: &'static str = "persist_model_tool_response";
    const ADVANCE_TOOL_CALL: &'static str = "advance_tool_call";
    const PERSIST_TOOL_RESULT: &'static str = "persist_tool_result";
    const FINISH_TOOL_CALL: &'static str = "finish_tool_call";
    const RECORD_USAGE: &'static str = "record_usage";
    const COMPLETE_TURN: &'static str = "complete_turn";
    const FINISH_TURN: &'static str = "finish_turn";
    const REPLACE_HISTORY: &'static str = "replace_history";

    const fn as_str(self) -> &'static str {
        match self {
            Self::InspectSession => Self::INSPECT_SESSION,
            Self::CreateSession => Self::CREATE_SESSION,
            Self::RestoreMembership => Self::RESTORE_MEMBERSHIP,
            Self::RestoreHistory => Self::RESTORE_HISTORY,
            Self::BeginTurn => Self::BEGIN_TURN,
            Self::BeginToolCall => Self::BEGIN_TOOL_CALL,
            Self::PersistModelToolResponse => Self::PERSIST_MODEL_TOOL_RESPONSE,
            Self::AdvanceToolCall => Self::ADVANCE_TOOL_CALL,
            Self::PersistToolResult => Self::PERSIST_TOOL_RESULT,
            Self::FinishToolCall => Self::FINISH_TOOL_CALL,
            Self::RecordUsage => Self::RECORD_USAGE,
            Self::CompleteTurn => Self::COMPLETE_TURN,
            Self::FinishTurn => Self::FINISH_TURN,
            Self::ReplaceHistory => Self::REPLACE_HISTORY,
        }
    }
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
    CoordinationTransition {
        session_id: SessionId,
        outcome: RuntimeCoordinationOutcome,
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
    /// Boot recovery durably classified one interrupted tool invocation.
    ToolRecoveryClassified {
        turn_id: String,
        session_id: SessionId,
        tool_call_id: String,
        position: ToolExecutionPosition,
        decision: ToolRecoveryDecision,
        operator_action_required: bool,
    },
    /// Boot recovery durably classified one interrupted model iteration.
    ModelRecoveryClassified {
        turn_id: String,
        session_id: SessionId,
        position: ModelExecutionPosition,
        decision: ModelRecoveryDecision,
        operator_action_required: bool,
    },
    /// Boot recovery durably classified one interrupted perception invocation.
    PerceptionRecoveryClassified {
        turn_id: String,
        session_id: SessionId,
        invocation_id: String,
        position: PerceptionExecutionPosition,
        decision: PerceptionRecoveryDecision,
        operator_action_required: bool,
    },
    /// One perception specialist invocation reached a terminal.
    PerceptionEvaluationFinished {
        turn_id: String,
        session_id: SessionId,
        invocation_id: String,
        succeeded: bool,
        recovered_from_receipt: bool,
        automatic: bool,
    },
    CognitionRecoveryClassified {
        turn_id: String,
        session_id: SessionId,
        invocation_id: String,
        position: CognitionExecutionPosition,
        decision: CognitionRecoveryDecision,
    },
    CognitionConsultationFinished {
        turn_id: String,
        session_id: SessionId,
        invocation_id: String,
        succeeded: bool,
        recovered_from_receipt: bool,
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
    const CHAT_ADMITTED: &'static str = "chat_admitted";
    const CHAT_DISPATCH_FINISHED: &'static str = "chat_dispatch_finished";
    const COORDINATION_TRANSITION: &'static str = "coordination_transition";
    const TURN_STARTED: &'static str = "turn_started";
    const TURN_TRANSITIONED: &'static str = "turn_transitioned";
    const MODEL_RETRIED: &'static str = "model_retried";
    const TOOL_STARTED: &'static str = "tool_started";
    const TOOL_FINISHED: &'static str = "tool_finished";
    const TOOL_RECOVERY_CLASSIFIED: &'static str = "tool_recovery_classified";
    const MODEL_RECOVERY_CLASSIFIED: &'static str = "model_recovery_classified";
    const PERCEPTION_RECOVERY_CLASSIFIED: &'static str = "perception_recovery_classified";
    const PERCEPTION_EVALUATION_FINISHED: &'static str = "perception_evaluation_finished";
    const COGNITION_RECOVERY_CLASSIFIED: &'static str = "cognition_recovery_classified";
    const COGNITION_CONSULTATION_FINISHED: &'static str = "cognition_consultation_finished";
    const PERSISTENCE_FINISHED: &'static str = "persistence_finished";
    const TURN_COMPLETED: &'static str = "turn_completed";
    const TURN_INTERRUPTED: &'static str = "turn_interrupted";
    const TURN_FAILED: &'static str = "turn_failed";

    const fn as_str(&self) -> &'static str {
        match self {
            Self::ChatAdmitted { .. } => Self::CHAT_ADMITTED,
            Self::ChatDispatchFinished { .. } => Self::CHAT_DISPATCH_FINISHED,
            Self::CoordinationTransition { .. } => Self::COORDINATION_TRANSITION,
            Self::TurnStarted { .. } => Self::TURN_STARTED,
            Self::TurnTransitioned { .. } => Self::TURN_TRANSITIONED,
            Self::ModelRetried { .. } => Self::MODEL_RETRIED,
            Self::ToolStarted { .. } => Self::TOOL_STARTED,
            Self::ToolFinished { .. } => Self::TOOL_FINISHED,
            Self::ToolRecoveryClassified { .. } => Self::TOOL_RECOVERY_CLASSIFIED,
            Self::ModelRecoveryClassified { .. } => Self::MODEL_RECOVERY_CLASSIFIED,
            Self::PerceptionRecoveryClassified { .. } => Self::PERCEPTION_RECOVERY_CLASSIFIED,
            Self::PerceptionEvaluationFinished { .. } => Self::PERCEPTION_EVALUATION_FINISHED,
            Self::CognitionRecoveryClassified { .. } => Self::COGNITION_RECOVERY_CLASSIFIED,
            Self::CognitionConsultationFinished { .. } => Self::COGNITION_CONSULTATION_FINISHED,
            Self::PersistenceFinished { .. } => Self::PERSISTENCE_FINISHED,
            Self::TurnCompleted { .. } => Self::TURN_COMPLETED,
            Self::TurnInterrupted { .. } => Self::TURN_INTERRUPTED,
            Self::TurnFailed { .. } => Self::TURN_FAILED,
        }
    }

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
    /// Interrupted tool calls durably classified during boot recovery.
    pub tool_recoveries_classified: u64,
    /// Classified calls that require an explicit operator decision.
    pub tool_recoveries_manual: u64,
    /// Interrupted model iterations durably classified during boot recovery.
    pub model_recoveries_classified: u64,
    /// Model iterations that require an explicit operator decision.
    pub model_recoveries_manual: u64,
    /// Interrupted perception invocations durably classified during boot recovery.
    pub perception_recoveries_classified: u64,
    /// Perception invocations that require an explicit operator decision.
    pub perception_recoveries_manual: u64,
    /// Explicit same-Agent perception evaluations attempted.
    pub perception_evaluations: u64,
    /// Perception evaluations that produced a durable model-visible result.
    pub perception_evaluations_succeeded: u64,
    /// Perception evaluations that ended content-safely without a result.
    pub perception_evaluations_failed: u64,
    /// Approved perception routes attempted automatically inside admitted turns.
    pub perception_automatic_routes: u64,
    /// Automatic perception routes that produced a model-visible observation.
    pub perception_automatic_routes_succeeded: u64,
    /// Automatic routes that degraded to the content-safe unavailable marker.
    pub perception_automatic_routes_soft_failed: u64,
    /// Successful evaluations reconstructed from a durable provider receipt.
    pub perception_receipts_recovered: u64,
    pub cognition_recoveries_classified: u64,
    pub cognition_consultations: u64,
    pub cognition_consultations_succeeded: u64,
    pub cognition_consultations_failed: u64,
    pub cognition_receipts_recovered: u64,
    /// Explicit filesystem-boundary policy denials reported by an adapter.
    pub filesystem_policy_violations: u64,
    /// Required Session persistence operations that committed.
    pub persistence_succeeded: u64,
    /// Required Session persistence operations that failed.
    pub persistence_failed: u64,
    pub coordination_enqueued: u64,
    pub coordination_tasks_created: u64,
    pub coordination_tasks_transitioned: u64,
    pub coordination_tasks_claimed: u64,
    pub coordination_task_leases_recovered: u64,
    pub coordination_tasks_cancelled: u64,
    pub coordination_background_dispatches_recovered: u64,
    pub coordination_participant_activated: u64,
    pub coordination_topology_updated: u64,
    pub coordination_arbitration_required: u64,
    pub coordination_moderator_authorized: u64,
    pub coordination_moderator_rejected: u64,
    pub coordination_arbitration_applied: u64,
    pub coordination_mailbox_escalated: u64,
    pub workspace_reviews_prepared: u64,
    pub workspace_integrations_approved: u64,
    pub workspace_integrations_applied: u64,
    pub workspace_merges_recovered: u64,
    pub workspace_integrations_conflicted: u64,
    pub recovery_actions_abandoned: u64,
    pub recovery_retries_authorized: u64,
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
    observations: broadcast::Sender<RuntimeEvent>,
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
    tool_recoveries_classified: AtomicU64,
    tool_recoveries_manual: AtomicU64,
    model_recoveries_classified: AtomicU64,
    model_recoveries_manual: AtomicU64,
    perception_recoveries_classified: AtomicU64,
    perception_recoveries_manual: AtomicU64,
    perception_evaluations: AtomicU64,
    perception_evaluations_succeeded: AtomicU64,
    perception_evaluations_failed: AtomicU64,
    perception_automatic_routes: AtomicU64,
    perception_automatic_routes_succeeded: AtomicU64,
    perception_automatic_routes_soft_failed: AtomicU64,
    perception_receipts_recovered: AtomicU64,
    cognition_recoveries_classified: AtomicU64,
    cognition_consultations: AtomicU64,
    cognition_consultations_succeeded: AtomicU64,
    cognition_consultations_failed: AtomicU64,
    cognition_receipts_recovered: AtomicU64,
    filesystem_policy_violations: AtomicU64,
    persistence_succeeded: AtomicU64,
    persistence_failed: AtomicU64,
    coordination_enqueued: AtomicU64,
    coordination_tasks_created: AtomicU64,
    coordination_tasks_transitioned: AtomicU64,
    coordination_tasks_claimed: AtomicU64,
    coordination_task_leases_recovered: AtomicU64,
    coordination_tasks_cancelled: AtomicU64,
    coordination_background_dispatches_recovered: AtomicU64,
    coordination_participant_activated: AtomicU64,
    coordination_topology_updated: AtomicU64,
    coordination_arbitration_required: AtomicU64,
    coordination_moderator_authorized: AtomicU64,
    coordination_moderator_rejected: AtomicU64,
    coordination_arbitration_applied: AtomicU64,
    coordination_mailbox_escalated: AtomicU64,
    workspace_reviews_prepared: AtomicU64,
    workspace_integrations_approved: AtomicU64,
    workspace_integrations_applied: AtomicU64,
    workspace_merges_recovered: AtomicU64,
    workspace_integrations_conflicted: AtomicU64,
    recovery_actions_abandoned: AtomicU64,
    recovery_retries_authorized: AtomicU64,
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
        let (observations, _) = broadcast::channel(RUNTIME_OBSERVATION_BUFFER_CAPACITY);
        Self {
            inner: Arc::new(RuntimeObservabilityInner {
                clock,
                observations,
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
                tool_recoveries_classified: AtomicU64::new(0),
                tool_recoveries_manual: AtomicU64::new(0),
                model_recoveries_classified: AtomicU64::new(0),
                model_recoveries_manual: AtomicU64::new(0),
                perception_recoveries_classified: AtomicU64::new(0),
                perception_recoveries_manual: AtomicU64::new(0),
                perception_evaluations: AtomicU64::new(0),
                perception_evaluations_succeeded: AtomicU64::new(0),
                perception_evaluations_failed: AtomicU64::new(0),
                perception_automatic_routes: AtomicU64::new(0),
                perception_automatic_routes_succeeded: AtomicU64::new(0),
                perception_automatic_routes_soft_failed: AtomicU64::new(0),
                perception_receipts_recovered: AtomicU64::new(0),
                cognition_recoveries_classified: AtomicU64::new(0),
                cognition_consultations: AtomicU64::new(0),
                cognition_consultations_succeeded: AtomicU64::new(0),
                cognition_consultations_failed: AtomicU64::new(0),
                cognition_receipts_recovered: AtomicU64::new(0),
                filesystem_policy_violations: AtomicU64::new(0),
                persistence_succeeded: AtomicU64::new(0),
                persistence_failed: AtomicU64::new(0),
                coordination_enqueued: AtomicU64::new(0),
                coordination_tasks_created: AtomicU64::new(0),
                coordination_tasks_transitioned: AtomicU64::new(0),
                coordination_tasks_claimed: AtomicU64::new(0),
                coordination_task_leases_recovered: AtomicU64::new(0),
                coordination_tasks_cancelled: AtomicU64::new(0),
                coordination_background_dispatches_recovered: AtomicU64::new(0),
                coordination_participant_activated: AtomicU64::new(0),
                coordination_topology_updated: AtomicU64::new(0),
                coordination_arbitration_required: AtomicU64::new(0),
                coordination_moderator_authorized: AtomicU64::new(0),
                coordination_moderator_rejected: AtomicU64::new(0),
                coordination_arbitration_applied: AtomicU64::new(0),
                coordination_mailbox_escalated: AtomicU64::new(0),
                workspace_reviews_prepared: AtomicU64::new(0),
                workspace_integrations_approved: AtomicU64::new(0),
                workspace_integrations_applied: AtomicU64::new(0),
                workspace_merges_recovered: AtomicU64::new(0),
                workspace_integrations_conflicted: AtomicU64::new(0),
                recovery_actions_abandoned: AtomicU64::new(0),
                recovery_retries_authorized: AtomicU64::new(0),
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
        let observation = event.clone();
        let event_name = event.as_str();
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
                    event = event_name,
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
                    event = event_name,
                    %request_id,
                    %session_id,
                    succeeded,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::CoordinationTransition {
                session_id,
                outcome,
            } => {
                match outcome {
                    RuntimeCoordinationOutcome::Enqueued => &self.inner.coordination_enqueued,
                    RuntimeCoordinationOutcome::TaskCreated => {
                        &self.inner.coordination_tasks_created
                    }
                    RuntimeCoordinationOutcome::TaskTransitioned => {
                        &self.inner.coordination_tasks_transitioned
                    }
                    RuntimeCoordinationOutcome::TaskClaimed => {
                        &self.inner.coordination_tasks_claimed
                    }
                    RuntimeCoordinationOutcome::TaskLeaseRecovered => {
                        &self.inner.coordination_task_leases_recovered
                    }
                    RuntimeCoordinationOutcome::TaskCancelled => {
                        &self.inner.coordination_tasks_cancelled
                    }
                    RuntimeCoordinationOutcome::BackgroundDispatchRecovered => {
                        &self.inner.coordination_background_dispatches_recovered
                    }
                    RuntimeCoordinationOutcome::ParticipantActivated => {
                        &self.inner.coordination_participant_activated
                    }
                    RuntimeCoordinationOutcome::TopologyUpdated => {
                        &self.inner.coordination_topology_updated
                    }
                    RuntimeCoordinationOutcome::ArbitrationRequired => {
                        &self.inner.coordination_arbitration_required
                    }
                    RuntimeCoordinationOutcome::ModeratorAuthorized => {
                        &self.inner.coordination_moderator_authorized
                    }
                    RuntimeCoordinationOutcome::ModeratorRejected => {
                        &self.inner.coordination_moderator_rejected
                    }
                    RuntimeCoordinationOutcome::ArbitrationApplied => {
                        &self.inner.coordination_arbitration_applied
                    }
                    RuntimeCoordinationOutcome::MailboxEscalated => {
                        &self.inner.coordination_mailbox_escalated
                    }
                    RuntimeCoordinationOutcome::WorkspaceReviewPrepared => {
                        &self.inner.workspace_reviews_prepared
                    }
                    RuntimeCoordinationOutcome::WorkspaceApproved => {
                        &self.inner.workspace_integrations_approved
                    }
                    RuntimeCoordinationOutcome::WorkspaceApplied => {
                        &self.inner.workspace_integrations_applied
                    }
                    RuntimeCoordinationOutcome::WorkspaceMergeRecovered => {
                        &self.inner.workspace_merges_recovered
                    }
                    RuntimeCoordinationOutcome::WorkspaceConflicted => {
                        &self.inner.workspace_integrations_conflicted
                    }
                    RuntimeCoordinationOutcome::RecoveryAbandoned => {
                        &self.inner.recovery_actions_abandoned
                    }
                    RuntimeCoordinationOutcome::RecoveryRetryAuthorized => {
                        &self.inner.recovery_retries_authorized
                    }
                }
                .fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = event_name,
                    %session_id,
                    outcome = outcome.as_str(),
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
                    event = event_name,
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
                    event = event_name,
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
                    event = event_name,
                    %turn_id,
                    %session_id,
                    sequence = transition.sequence,
                    iteration = transition.iteration,
                    from = transition.from.as_str(),
                    to = transition.to.as_str(),
                    reason = transition.reason.as_str(),
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
                    event = event_name,
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
                    event = event_name,
                    %turn_id,
                    %session_id,
                    %tool_call_id,
                    %tool_name,
                    succeeded,
                    ?failure_kind,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::ToolRecoveryClassified {
                turn_id,
                session_id,
                tool_call_id,
                position,
                decision,
                operator_action_required,
            } => {
                self.inner
                    .tool_recoveries_classified
                    .fetch_add(1, Ordering::Relaxed);
                if operator_action_required {
                    self.inner
                        .tool_recoveries_manual
                        .fetch_add(1, Ordering::Relaxed);
                }
                tracing::info!(
                    event = event_name,
                    %turn_id,
                    %session_id,
                    %tool_call_id,
                    ?position,
                    ?decision,
                    operator_action_required,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::ModelRecoveryClassified {
                turn_id,
                session_id,
                position,
                decision,
                operator_action_required,
            } => {
                self.inner
                    .model_recoveries_classified
                    .fetch_add(1, Ordering::Relaxed);
                if operator_action_required {
                    self.inner
                        .model_recoveries_manual
                        .fetch_add(1, Ordering::Relaxed);
                }
                tracing::info!(
                    event = event_name,
                    %turn_id,
                    %session_id,
                    ?position,
                    ?decision,
                    operator_action_required,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::PerceptionRecoveryClassified {
                turn_id,
                session_id,
                invocation_id,
                position,
                decision,
                operator_action_required,
            } => {
                self.inner
                    .perception_recoveries_classified
                    .fetch_add(1, Ordering::Relaxed);
                if operator_action_required {
                    self.inner
                        .perception_recoveries_manual
                        .fetch_add(1, Ordering::Relaxed);
                }
                tracing::info!(
                    event = event_name,
                    %turn_id,
                    %session_id,
                    %invocation_id,
                    ?position,
                    ?decision,
                    operator_action_required,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::PerceptionEvaluationFinished {
                turn_id,
                session_id,
                invocation_id,
                succeeded,
                recovered_from_receipt,
                automatic,
            } => {
                if automatic {
                    self.inner
                        .perception_automatic_routes
                        .fetch_add(1, Ordering::Relaxed);
                    if succeeded {
                        self.inner
                            .perception_automatic_routes_succeeded
                            .fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.inner
                            .perception_automatic_routes_soft_failed
                            .fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    self.inner
                        .perception_evaluations
                        .fetch_add(1, Ordering::Relaxed);
                    if succeeded {
                        self.inner
                            .perception_evaluations_succeeded
                            .fetch_add(1, Ordering::Relaxed);
                    } else {
                        self.inner
                            .perception_evaluations_failed
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
                if recovered_from_receipt && succeeded {
                    self.inner
                        .perception_receipts_recovered
                        .fetch_add(1, Ordering::Relaxed);
                }
                tracing::info!(
                    event = event_name,
                    %turn_id,
                    %session_id,
                    %invocation_id,
                    succeeded,
                    recovered_from_receipt,
                    automatic,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::CognitionRecoveryClassified {
                turn_id,
                session_id,
                invocation_id,
                position,
                decision,
            } => {
                self.inner
                    .cognition_recoveries_classified
                    .fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    event = event_name,
                    %turn_id,
                    %session_id,
                    %invocation_id,
                    ?position,
                    ?decision,
                    "runtime lifecycle fact"
                );
            }
            RuntimeEvent::CognitionConsultationFinished {
                turn_id,
                session_id,
                invocation_id,
                succeeded,
                recovered_from_receipt,
            } => {
                self.inner
                    .cognition_consultations
                    .fetch_add(1, Ordering::Relaxed);
                if succeeded {
                    self.inner
                        .cognition_consultations_succeeded
                        .fetch_add(1, Ordering::Relaxed);
                } else {
                    self.inner
                        .cognition_consultations_failed
                        .fetch_add(1, Ordering::Relaxed);
                }
                if succeeded && recovered_from_receipt {
                    self.inner
                        .cognition_receipts_recovered
                        .fetch_add(1, Ordering::Relaxed);
                }
                tracing::info!(
                    event = event_name,
                    %turn_id,
                    %session_id,
                    %invocation_id,
                    succeeded,
                    recovered_from_receipt,
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
                    event = event_name,
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
                    event = event_name,
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
                    event = event_name,
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
                    event = event_name,
                    %turn_id,
                    %session_id,
                    kind = ?kind,
                    "runtime lifecycle fact"
                );
            }
        }
        // Governance consumers are deliberately lossy and never apply
        // backpressure to the execution path. No subscribers is not an error.
        let _ = self.inner.observations.send(observation);
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.inner.observations.subscribe()
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
                timing
                    .tool_started
                    .retain(|(active_session_id, active_turn_id, _), _| {
                        active_session_id != session_id || active_turn_id != turn_id
                    });
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
            | RuntimeEvent::CoordinationTransition { .. }
            | RuntimeEvent::ModelRetried { .. }
            | RuntimeEvent::PersistenceFinished { .. }
            | RuntimeEvent::ToolRecoveryClassified { .. }
            | RuntimeEvent::ModelRecoveryClassified { .. }
            | RuntimeEvent::PerceptionRecoveryClassified { .. }
            | RuntimeEvent::PerceptionEvaluationFinished { .. }
            | RuntimeEvent::CognitionRecoveryClassified { .. }
            | RuntimeEvent::CognitionConsultationFinished { .. } => {}
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
            tool_recoveries_classified: self
                .inner
                .tool_recoveries_classified
                .load(Ordering::Relaxed),
            tool_recoveries_manual: self.inner.tool_recoveries_manual.load(Ordering::Relaxed),
            model_recoveries_classified: self
                .inner
                .model_recoveries_classified
                .load(Ordering::Relaxed),
            model_recoveries_manual: self.inner.model_recoveries_manual.load(Ordering::Relaxed),
            perception_recoveries_classified: self
                .inner
                .perception_recoveries_classified
                .load(Ordering::Relaxed),
            perception_recoveries_manual: self
                .inner
                .perception_recoveries_manual
                .load(Ordering::Relaxed),
            perception_evaluations: self.inner.perception_evaluations.load(Ordering::Relaxed),
            perception_evaluations_succeeded: self
                .inner
                .perception_evaluations_succeeded
                .load(Ordering::Relaxed),
            perception_evaluations_failed: self
                .inner
                .perception_evaluations_failed
                .load(Ordering::Relaxed),
            perception_automatic_routes: self
                .inner
                .perception_automatic_routes
                .load(Ordering::Relaxed),
            perception_automatic_routes_succeeded: self
                .inner
                .perception_automatic_routes_succeeded
                .load(Ordering::Relaxed),
            perception_automatic_routes_soft_failed: self
                .inner
                .perception_automatic_routes_soft_failed
                .load(Ordering::Relaxed),
            perception_receipts_recovered: self
                .inner
                .perception_receipts_recovered
                .load(Ordering::Relaxed),
            cognition_recoveries_classified: self
                .inner
                .cognition_recoveries_classified
                .load(Ordering::Relaxed),
            cognition_consultations: self.inner.cognition_consultations.load(Ordering::Relaxed),
            cognition_consultations_succeeded: self
                .inner
                .cognition_consultations_succeeded
                .load(Ordering::Relaxed),
            cognition_consultations_failed: self
                .inner
                .cognition_consultations_failed
                .load(Ordering::Relaxed),
            cognition_receipts_recovered: self
                .inner
                .cognition_receipts_recovered
                .load(Ordering::Relaxed),
            filesystem_policy_violations: self
                .inner
                .filesystem_policy_violations
                .load(Ordering::Relaxed),
            persistence_succeeded: self.inner.persistence_succeeded.load(Ordering::Relaxed),
            persistence_failed: self.inner.persistence_failed.load(Ordering::Relaxed),
            coordination_enqueued: self.inner.coordination_enqueued.load(Ordering::Relaxed),
            coordination_tasks_created: self
                .inner
                .coordination_tasks_created
                .load(Ordering::Relaxed),
            coordination_tasks_transitioned: self
                .inner
                .coordination_tasks_transitioned
                .load(Ordering::Relaxed),
            coordination_tasks_claimed: self
                .inner
                .coordination_tasks_claimed
                .load(Ordering::Relaxed),
            coordination_task_leases_recovered: self
                .inner
                .coordination_task_leases_recovered
                .load(Ordering::Relaxed),
            coordination_tasks_cancelled: self
                .inner
                .coordination_tasks_cancelled
                .load(Ordering::Relaxed),
            coordination_background_dispatches_recovered: self
                .inner
                .coordination_background_dispatches_recovered
                .load(Ordering::Relaxed),
            coordination_participant_activated: self
                .inner
                .coordination_participant_activated
                .load(Ordering::Relaxed),
            coordination_topology_updated: self
                .inner
                .coordination_topology_updated
                .load(Ordering::Relaxed),
            coordination_arbitration_required: self
                .inner
                .coordination_arbitration_required
                .load(Ordering::Relaxed),
            coordination_moderator_authorized: self
                .inner
                .coordination_moderator_authorized
                .load(Ordering::Relaxed),
            coordination_moderator_rejected: self
                .inner
                .coordination_moderator_rejected
                .load(Ordering::Relaxed),
            coordination_arbitration_applied: self
                .inner
                .coordination_arbitration_applied
                .load(Ordering::Relaxed),
            coordination_mailbox_escalated: self
                .inner
                .coordination_mailbox_escalated
                .load(Ordering::Relaxed),
            workspace_reviews_prepared: self
                .inner
                .workspace_reviews_prepared
                .load(Ordering::Relaxed),
            workspace_integrations_approved: self
                .inner
                .workspace_integrations_approved
                .load(Ordering::Relaxed),
            workspace_integrations_applied: self
                .inner
                .workspace_integrations_applied
                .load(Ordering::Relaxed),
            workspace_merges_recovered: self
                .inner
                .workspace_merges_recovered
                .load(Ordering::Relaxed),
            workspace_integrations_conflicted: self
                .inner
                .workspace_integrations_conflicted
                .load(Ordering::Relaxed),
            recovery_actions_abandoned: self
                .inner
                .recovery_actions_abandoned
                .load(Ordering::Relaxed),
            recovery_retries_authorized: self
                .inner
                .recovery_retries_authorized
                .load(Ordering::Relaxed),
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
