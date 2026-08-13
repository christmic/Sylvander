//! Explicit state machine for one bounded Agent turn.
//!
//! The machine owns authoritative turn-local state. Streaming events are
//! projections of its transitions; they are not the source of truth.

use sylvander_llm_core::{ChatMessage, ModelResponse, StopReason, TokenUsage};

use crate::turn::conversation::ConversationSnapshot;
use crate::turn::outcome::AgentOutcome;

/// Stable phase names for the execution of one Agent turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnPhase {
    Created,
    Validating,
    RunningBeforeHooks,
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
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Interrupted)
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
    use TurnPhase as P;
    use TurnTransitionReason as R;

    matches!(
        (from, to, reason),
        (P::Created, P::Validating, R::ExecutionStarted)
            | (P::Validating, P::RunningBeforeHooks, R::RequestValidated)
            | (
                P::RunningBeforeHooks,
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
            | (P::PreparingTools, P::WaitingForUser, R::UserInputRequired)
            | (P::WaitingForUser, P::ExecutingTools, R::UserInputResolved)
            | (
                P::PreparingTools,
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
                P::ExecutingTools,
                P::PreparingIteration,
                R::ContinueAfterToolResults
            )
            | (
                P::FinalizingModelResponse,
                P::PreparingIteration,
                R::ContinueAfterMaxOutputTokens
            )
            | (
                P::FinalizingModelResponse,
                P::RunningAfterHooks,
                R::TerminalModelResponse
            )
            | (
                P::ExecutingTools,
                P::RunningAfterHooks,
                R::TerminalModelResponse
            )
            | (P::RunningAfterHooks, P::Completed, R::AfterHooksCompleted)
            | (_, P::Failed, R::ExecutionFailed)
            | (_, P::Interrupted, R::InterruptRequested)
    )
}
