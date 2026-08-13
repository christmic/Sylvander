//! Explicit state machine for one bounded Agent turn.
//!
//! The machine owns authoritative turn-local state. Streaming events are
//! projections of its transitions; they are not the source of truth.

use sylvander_llm_core::{ChatMessage, ModelResponse, StopReason, TokenUsage};

use crate::turn::conversation::ConversationSnapshot;
use crate::turn::outcome::AgentOutcome;
use TurnPhase as P;
use TurnTransitionReason as R;

/// Stable phase names for the execution of one Agent turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnPhase {
    Created,
    Validating,
    RunningBeforeHooks,
    ReadyForIteration,
    PreparingIteration,
    Compacting,
    CallingModel,
    StreamingModel,
    FinalizingModelResponse,
    PreparingTools,
    WaitingForApproval,
    WaitingForUser,
    WaitingForPlanReview,
    ExecutingTools,
    RunningAfterHooks,
    Completed,
    Failed,
    Interrupted,
}

impl TurnPhase {
    pub const CREATED: &'static str = "created";
    pub const VALIDATING: &'static str = "validating";
    pub const RUNNING_BEFORE_HOOKS: &'static str = "running_before_hooks";
    pub const READY_FOR_ITERATION: &'static str = "ready_for_iteration";
    pub const PREPARING_ITERATION: &'static str = "preparing_iteration";
    pub const COMPACTING: &'static str = "compacting";
    pub const CALLING_MODEL: &'static str = "calling_model";
    pub const STREAMING_MODEL: &'static str = "streaming_model";
    pub const FINALIZING_MODEL_RESPONSE: &'static str = "finalizing_model_response";
    pub const PREPARING_TOOLS: &'static str = "preparing_tools";
    pub const WAITING_FOR_APPROVAL: &'static str = "waiting_for_approval";
    pub const WAITING_FOR_USER: &'static str = "waiting_for_user";
    pub const WAITING_FOR_PLAN_REVIEW: &'static str = "waiting_for_plan_review";
    pub const EXECUTING_TOOLS: &'static str = "executing_tools";
    pub const RUNNING_AFTER_HOOKS: &'static str = "running_after_hooks";
    pub const COMPLETED: &'static str = "completed";
    pub const FAILED: &'static str = "failed";
    pub const INTERRUPTED: &'static str = "interrupted";

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Interrupted)
    }

    /// Stable projection name. Persistence, telemetry, and API adapters use
    /// this centralized vocabulary instead of defining their own strings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => Self::CREATED,
            Self::Validating => Self::VALIDATING,
            Self::RunningBeforeHooks => Self::RUNNING_BEFORE_HOOKS,
            Self::ReadyForIteration => Self::READY_FOR_ITERATION,
            Self::PreparingIteration => Self::PREPARING_ITERATION,
            Self::Compacting => Self::COMPACTING,
            Self::CallingModel => Self::CALLING_MODEL,
            Self::StreamingModel => Self::STREAMING_MODEL,
            Self::FinalizingModelResponse => Self::FINALIZING_MODEL_RESPONSE,
            Self::PreparingTools => Self::PREPARING_TOOLS,
            Self::WaitingForApproval => Self::WAITING_FOR_APPROVAL,
            Self::WaitingForUser => Self::WAITING_FOR_USER,
            Self::WaitingForPlanReview => Self::WAITING_FOR_PLAN_REVIEW,
            Self::ExecutingTools => Self::EXECUTING_TOOLS,
            Self::RunningAfterHooks => Self::RUNNING_AFTER_HOOKS,
            Self::Completed => Self::COMPLETED,
            Self::Failed => Self::FAILED,
            Self::Interrupted => Self::INTERRUPTED,
        }
    }
}

/// Typed explanation for one legal phase transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnTransitionReason {
    ExecutionStarted,
    RequestValidated,
    BeforeHooksCompleted,
    IterationStarted,
    CompressionStarted,
    CompressionCompleted,
    ModelCallStarted,
    ModelStreamOpened,
    ModelResponseCompleted,
    ToolPreparationStarted,
    ApprovalRequired,
    ApprovalResolved,
    UserInputRequired,
    UserInputResolved,
    PlanReviewRequired,
    PlanReviewResolved,
    ToolExecutionStarted,
    ToolExecutionCompleted,
    ContinueAfterToolResults,
    ContinueAfterMaxOutputTokens,
    TerminalModelResponse,
    IterationLimitReached,
    AfterHooksCompleted,
    ExecutionFailed,
    InterruptRequested,
}

/// Why the machine will run another model iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnContinuationReason {
    ToolResultsReady,
    MaxOutputTokens,
    ProviderRequestedContinuation,
}

impl TurnContinuationReason {
    pub const TOOL_RESULTS_READY: &'static str = "tool_results_ready";
    pub const MAX_OUTPUT_TOKENS: &'static str = "max_output_tokens";
    pub const PROVIDER_REQUESTED_CONTINUATION: &'static str = "provider_requested_continuation";

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ToolResultsReady => Self::TOOL_RESULTS_READY,
            Self::MaxOutputTokens => Self::MAX_OUTPUT_TOKENS,
            Self::ProviderRequestedContinuation => Self::PROVIDER_REQUESTED_CONTINUATION,
        }
    }
}

/// One monotonic state transition emitted by the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnTransition {
    pub sequence: u64,
    pub iteration: u32,
    pub from: TurnPhase,
    pub to: TurnPhase,
    pub reason: TurnTransitionReason,
}

/// Content-free current-state view for Runtime observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnSnapshot {
    pub sequence: u64,
    pub iteration: u32,
    pub phase: TurnPhase,
    pub continuation: Option<TurnContinuationReason>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TurnStateError {
    #[error("turn state transition is not allowed: {from:?} -> {to:?} ({reason:?})")]
    InvalidTransition {
        from: TurnPhase,
        to: TurnPhase,
        reason: TurnTransitionReason,
    },
    #[error("terminal turn state cannot transition")]
    TerminalTransition,
    #[error("turn iteration must advance monotonically")]
    InvalidIteration,
    #[error("turn outcome is unavailable before a model response")]
    MissingFinalResponse,
}

/// Authoritative mutable state for exactly one bounded turn.
pub struct TurnMachine {
    phase: TurnPhase,
    sequence: u64,
    iteration: u32,
    continuation: Option<TurnContinuationReason>,
    messages: Vec<ChatMessage>,
    cumulative_usage: TokenUsage,
    last_provider_usage: TokenUsage,
    final_response: Option<ModelResponse>,
    completed_iterations: u32,
}

impl TurnMachine {
    #[must_use]
    pub fn new(conversation: &ConversationSnapshot) -> Self {
        Self {
            phase: TurnPhase::Created,
            sequence: 0,
            iteration: 0,
            continuation: None,
            messages: conversation.messages().to_vec(),
            cumulative_usage: TokenUsage::default(),
            last_provider_usage: TokenUsage::default(),
            final_response: None,
            completed_iterations: 0,
        }
    }

    #[must_use]
    pub const fn snapshot(&self) -> TurnSnapshot {
        TurnSnapshot {
            sequence: self.sequence,
            iteration: self.iteration,
            phase: self.phase,
            continuation: self.continuation,
        }
    }

    pub fn transition(
        &mut self,
        to: TurnPhase,
        reason: TurnTransitionReason,
    ) -> Result<TurnTransition, TurnStateError> {
        if self.phase.is_terminal() {
            return Err(TurnStateError::TerminalTransition);
        }
        if !allowed(self.phase, to, reason) {
            return Err(TurnStateError::InvalidTransition {
                from: self.phase,
                to,
                reason,
            });
        }
        self.sequence = self.sequence.saturating_add(1);
        let transition = TurnTransition {
            sequence: self.sequence,
            iteration: self.iteration,
            from: self.phase,
            to,
            reason,
        };
        self.phase = to;
        Ok(transition)
    }

    pub fn start_iteration(&mut self, iteration: u32) -> Result<TurnTransition, TurnStateError> {
        if iteration != self.completed_iterations.saturating_add(1) {
            return Err(TurnStateError::InvalidIteration);
        }
        self.iteration = iteration;
        self.continuation = None;
        self.transition(
            TurnPhase::PreparingIteration,
            TurnTransitionReason::IterationStarted,
        )
    }

    pub fn complete_iteration(
        &mut self,
        response: ModelResponse,
        continuation: Option<TurnContinuationReason>,
    ) {
        self.last_provider_usage = response.usage;
        self.cumulative_usage
            .saturating_add_assign(self.last_provider_usage);
        self.completed_iterations = self.iteration;
        self.continuation = continuation;
        self.final_response = Some(ModelResponse {
            usage: self.cumulative_usage,
            ..response
        });
    }

    #[must_use]
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn messages_mut(&mut self) -> &mut Vec<ChatMessage> {
        &mut self.messages
    }

    #[must_use]
    pub const fn cumulative_usage(&self) -> TokenUsage {
        self.cumulative_usage
    }

    #[must_use]
    pub const fn last_provider_usage(&self) -> TokenUsage {
        self.last_provider_usage
    }

    pub fn outcome(&self) -> Result<AgentOutcome, TurnStateError> {
        let final_response = self
            .final_response
            .clone()
            .ok_or(TurnStateError::MissingFinalResponse)?;
        Ok(AgentOutcome {
            final_response,
            conversation: ConversationSnapshot::new(self.messages.clone()),
            iterations: self.completed_iterations,
            total_usage: self.cumulative_usage,
        })
    }

    #[must_use]
    pub fn continuation_for(response: &ModelResponse) -> Option<TurnContinuationReason> {
        match response.stop_reason {
            StopReason::ToolUse => Some(TurnContinuationReason::ToolResultsReady),
            StopReason::MaxOutputTokens => Some(TurnContinuationReason::MaxOutputTokens),
            StopReason::EndTurn
            | StopReason::StopSequence(_)
            | StopReason::Refusal
            | StopReason::Paused
            | StopReason::Other(_) => None,
        }
    }
}

const fn allowed(from: TurnPhase, to: TurnPhase, reason: TurnTransitionReason) -> bool {
    matches!(
        (from, to, reason),
        (P::Created, P::Validating, R::ExecutionStarted)
            | (P::Validating, P::RunningBeforeHooks, R::RequestValidated)
            | (
                P::RunningBeforeHooks,
                P::ReadyForIteration,
                R::BeforeHooksCompleted
            )
            | (
                P::ReadyForIteration,
                P::PreparingIteration,
                R::IterationStarted
            )
            | (P::PreparingIteration, P::Compacting, R::CompressionStarted)
            | (P::Compacting, P::CallingModel, R::CompressionCompleted)
            | (P::PreparingIteration, P::CallingModel, R::ModelCallStarted)
            | (P::CallingModel, P::StreamingModel, R::ModelStreamOpened)
            | (
                P::StreamingModel,
                P::FinalizingModelResponse,
                R::ModelResponseCompleted
            )
            | (
                P::FinalizingModelResponse,
                P::PreparingTools,
                R::ToolPreparationStarted
            )
            | (
                P::PreparingTools,
                P::WaitingForApproval,
                R::ApprovalRequired
            )
            | (
                P::WaitingForApproval,
                P::PreparingTools,
                R::ApprovalResolved
            )
            | (
                P::PreparingTools | P::ExecutingTools,
                P::WaitingForUser,
                R::UserInputRequired
            )
            | (P::WaitingForUser, P::ExecutingTools, R::UserInputResolved)
            | (
                P::PreparingTools | P::ExecutingTools,
                P::WaitingForPlanReview,
                R::PlanReviewRequired
            )
            | (
                P::WaitingForPlanReview,
                P::ExecutingTools,
                R::PlanReviewResolved
            )
            | (
                P::PreparingTools,
                P::ExecutingTools,
                R::ToolExecutionStarted
            )
            | (
                P::FinalizingModelResponse | P::ExecutingTools,
                P::ReadyForIteration,
                R::ContinueAfterToolResults | R::ContinueAfterMaxOutputTokens
            )
            | (
                P::FinalizingModelResponse | P::ExecutingTools,
                P::RunningAfterHooks,
                R::TerminalModelResponse
            )
            | (P::RunningAfterHooks, P::Completed, R::AfterHooksCompleted)
            | (
                P::ReadyForIteration,
                P::RunningAfterHooks,
                R::IterationLimitReached
            )
            | (_, P::Failed, R::ExecutionFailed)
            | (_, P::Interrupted, R::InterruptRequested)
    )
}

#[cfg(test)]
#[path = "../../tests/unit/turn_machine.rs"]
mod tests;
