//! `AgentLoop` — the OOP class-based async driver for the agent loop.
//!
//! # Architecture
//!
//! The loop logic lives in three module-level free functions:
//! - [`run`] — consumes the stream, returns `Result<AgentOutcome, _>`
//! - [`run_stream`] — the single source of truth: drives the
//!   iteration, yields `AgentEvent`s
//! - [`run_with_events`] — consumes the stream, fires events into a
//!   callback, returns the final `AgentOutcome`
//!
//! `AgentLoop` itself holds only stable retry, iteration, and compression
//! policy. Per-turn conversation, model, tools, and trusted execution identity
//! arrive in `AgentTurnRequest`; Runtime-selected model, authorization,
//! interaction, execution, and artifact services arrive separately in
//! `AgentExecutionPorts`.
//!
//! Adding new event types or consumption patterns only touches
//! `run_stream` — the single iteration implementation.
//!
//! See `sylvander-agent/docs/ARCHITECTURE.md` and
//! `sylvander-agent/docs/execution-kernel.md` for the current design.

use std::sync::Arc;

use futures_util::{Stream, StreamExt};
use sha2::{Digest as _, Sha256};
use tracing::{Instrument as _, warn};

use sylvander_llm_core::{
    ChatMessage, ContentBlock, ModelCapabilities, ModelEventStream, ModelInfo, ModelProvider,
    ModelRequest, ModelResponse, ProviderErrorKind, StopReason,
};

use crate::execution::ports::AgentExecutionPorts;
use crate::execution::tool_context::ToolContext;
use crate::interaction::approval::{ApprovalDecision, ToolUseRequest};
use crate::interaction::plan::PlanDecision;
use crate::tool::{AgentHookPhase, ToolRegistry};
use crate::turn::error::AgentLoopError;
use crate::turn::event::{AgentEvent, ModelRetryCause};
use crate::turn::machine::{
    TurnContinuationReason, TurnMachine, TurnPhase, TurnTransition, TurnTransitionReason,
};
use crate::turn::outcome::AgentOutcome;
use crate::turn::request::AgentTurnRequest;

/// Stable policy for the provider-neutral Agent execution kernel.
///
/// Model choice, transcript, tools, authority, and service implementations are
/// deliberately absent: Runtime freezes those volatile values into one
/// [`AgentTurnRequest`] and one [`AgentExecutionPorts`] value per execution.
/// Keeping only retry, iteration, and compression policy here prevents reused
/// kernels from leaking one Session's state or authority into another.
#[derive(Clone)]
pub struct AgentLoop {
    pub(crate) compression_pipeline:
        Option<Arc<crate::context::compression::pipeline::CompressionPipeline>>,
    pub(crate) max_iterations: u32,
    pub(crate) max_retries: u32,
}

struct LoopModelStream {
    stream: ModelEventStream,
    expected_model: sylvander_llm_core::ModelRef,
}

impl std::fmt::Debug for AgentLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoop")
            .field("custom_compression", &self.compression_pipeline.is_some())
            .field("max_iterations", &self.max_iterations)
            .field("max_retries", &self.max_retries)
            .finish_non_exhaustive()
    }
}

// =====================================================================
// Builder
// =====================================================================

/// Builder for [`AgentLoop`].
pub struct AgentLoopBuilder {
    compression_pipeline: Option<Arc<crate::context::compression::pipeline::CompressionPipeline>>,
    max_iterations: u32,
    max_retries: u32,
}

impl Default for AgentLoopBuilder {
    fn default() -> Self {
        Self {
            compression_pipeline: None,
            max_iterations: 50,
            max_retries: 3,
        }
    }
}

impl std::fmt::Debug for AgentLoopBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopBuilder")
            .field("custom_compression", &self.compression_pipeline.is_some())
            .field("max_iterations", &self.max_iterations)
            .field("max_retries", &self.max_retries)
            .finish_non_exhaustive()
    }
}

impl AgentLoopBuilder {
    /// Create a new builder with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the compression pipeline. If not called, defaults to
    /// [`CompressionPipeline::default_for_model`](crate::context::compression::pipeline::CompressionPipeline::default_for_model) (L1 + L2 + L3).
    /// Opt in to L0 or L4 by building a custom pipeline.
    #[must_use]
    pub fn compression_pipeline(
        mut self,
        pipeline: crate::context::compression::pipeline::CompressionPipeline,
    ) -> Self {
        self.compression_pipeline = Some(Arc::new(pipeline));
        self
    }

    /// Set the max iterations (default 50).
    #[must_use]
    pub fn max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }

    /// Set the max retries per LLM call (default 3). Set to 0 to
    /// disable retry.
    #[must_use]
    pub fn max_retries(mut self, n: u32) -> Self {
        self.max_retries = n;
        self
    }

    /// Build the stable loop policy.
    #[must_use]
    pub fn build(self) -> AgentLoop {
        AgentLoop {
            compression_pipeline: self.compression_pipeline,
            max_iterations: self.max_iterations,
            max_retries: self.max_retries,
        }
    }
}

// =====================================================================
// AgentLoop methods — accessor + builder + thin delegates
// =====================================================================

impl AgentLoop {
    /// Start building an agent loop.
    #[must_use]
    pub fn builder() -> AgentLoopBuilder {
        AgentLoopBuilder::new()
    }

    /// Configured max iterations.
    #[must_use]
    pub fn max_iterations(&self) -> u32 {
        self.max_iterations
    }

    /// Configured max retries per LLM call.
    #[must_use]
    pub fn max_retries(&self) -> u32 {
        self.max_retries
    }
}

// =====================================================================
// Free-function API — the canonical implementations
// =====================================================================

/// Drive the agent loop and yield events as they happen. The
/// single source of truth for iteration logic. `run` and
/// `run_with_events` consume the stream this returns.
///
/// `config` carries stable loop policy. `request` carries immutable turn data,
/// while `ports` carries the Runtime-selected service implementations and
/// executable authority for that exact turn.
///
/// Event order within an iteration:
/// `IterationStart → [Compressed] → [TextChunk* / ThinkingChunk*] →
/// [ToolCallStart → ToolCallEnd]* → IterationEnd → [repeat] → Done | Error`
/// A configured before-turn hook executes once before this sequence; a
/// configured after-turn hook executes once before the successful `Done`.
///
/// On error (capability mismatch, LLM failure after retries,
/// max iterations reached), yields an `AgentEvent::Error(_)` and
/// terminates the stream.
pub fn run_stream(
    config: &AgentLoop,
    request: AgentTurnRequest,
    ports: AgentExecutionPorts,
) -> impl Stream<Item = AgentEvent> + Send + '_ {
    let compression_pipeline = config.compression_pipeline.clone().unwrap_or_else(|| {
        Arc::new(
            crate::context::compression::pipeline::CompressionPipeline::default_for_model(
                &request.model,
            ),
        )
    });
    async_stream::stream! {
        let mut machine = TurnMachine::new(&request.conversation);
        yield AgentEvent::TurnTransition(required_turn_transition(
            &mut machine,
            TurnPhase::Validating,
            TurnTransitionReason::ExecutionStarted
        ));
        if let Err(error) = ports.validate_for(&request) {
            yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
            yield AgentEvent::Error(error);
            return;
        }
        yield AgentEvent::TurnTransition(required_turn_transition(
            &mut machine,
            TurnPhase::RunningBeforeHooks,
            TurnTransitionReason::RequestValidated
        ));

        if let Err(blocked) = request
            .tools
            .run_turn_hooks(AgentHookPhase::BeforeTurn, &ports.tool_context)
            .await
        {
            yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
            yield AgentEvent::Error(AgentLoopError::Tool(blocked.to_string()));
            return;
        }
        yield AgentEvent::TurnTransition(required_turn_transition(
            &mut machine,
            TurnPhase::ReadyForIteration,
            TurnTransitionReason::BeforeHooksCompleted
        ));

        for iteration in 1..=config.max_iterations {
            match machine.start_iteration(iteration) {
                Ok(transition) => yield AgentEvent::TurnTransition(transition),
                Err(error) => {
                    yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
                    yield AgentEvent::Error(AgentLoopError::Validation(error.to_string()));
                    return;
                }
            }
            yield AgentEvent::IterationStart { iteration };

            // 1. Compression (pipeline: layers run in order, async)
            {
                yield AgentEvent::TurnTransition(required_turn_transition(
                    &mut machine,
                    TurnPhase::Compacting,
                    TurnTransitionReason::CompressionStarted
                ));
                let auto_threshold = (request.model.context_window as f32
                    * crate::context::compression::layers::auto_compact::DEFAULT_TRIGGER_RATIO)
                    as u64;
                if machine.last_provider_usage().total_input_tokens() >= auto_threshold
                    && machine.messages().len() > 4
                {
                    yield AgentEvent::CompressionStarted;
                }
                let auto_llm = auto_compact_llm(&request, &ports);
                let last_provider_usage = machine.last_provider_usage();
                let mut compress_ctx = crate::context::compression::CompressContext {
                    messages: machine.messages_mut(),
                    last_usage: &last_provider_usage,
                    model_info: &request.model,
                    auto_compact_llm: Some(&auto_llm),
                    artifact_store: ports.artifact_store(),
                };
                let reports = compression_pipeline.run_all(&mut compress_ctx).await;
                // Filter out no-op reports (every layer runs every
                // iteration even when there's nothing to do — only
                // emit a Compressed event when at least one layer
                // actually did work or recorded a failure).
                let meaningful: Vec<_> = reports
                    .into_iter()
                    .filter(|r| {
                        r.removed_count > 0
                            || r.condensed_count > 0
                            || r.freed_tokens > 0
                            || r.failure.is_some()
                    })
                    .collect();
                if !meaningful.is_empty() {
                    yield AgentEvent::Compressed {
                        layers: meaningful.clone(),
                    };
                    yield AgentEvent::HistoryCompacted {
                        history: machine.messages().to_vec(),
                        layers: meaningful,
                    };
                }
            }
            yield AgentEvent::TurnTransition(required_turn_transition(
                &mut machine,
                TurnPhase::CallingModel,
                TurnTransitionReason::CompressionCompleted
            ));

            // 2. Build and validate one exact provider-qualified request.
            let provider_request = AgentLoop::build_provider_request(&request, machine.messages());
            let request_digest = match serde_json::to_vec(&provider_request) {
                Ok(encoded) => format!("sha256:{:x}", Sha256::digest(encoded)),
                Err(error) => {
                    yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
                    yield AgentEvent::Error(AgentLoopError::Validation(format!(
                        "provider-neutral request serialization failed: {error}"
                    )));
                    return;
                }
            };
            yield AgentEvent::ModelInvocationPrepared {
                iteration,
                invocation_id: provider_request.request_id.clone(),
                request_digest,
            };

            // 4. Open the provider stream. This is the only retry owner;
            //    provider adapters never retry and a failed streaming
            //    request is never replayed as a buffered request.
            let (retry_tx, mut retry_rx) = tokio::sync::mpsc::unbounded_channel();
            let call = config.call_model_with_retry(
                provider_request,
                retry_tx,
                ports.model.as_ref(),
                &request.model,
            );
            tokio::pin!(call);
            let call_result = loop {
                tokio::select! {
                    biased;
                    Some(retry) = retry_rx.recv() => yield retry,
                    result = &mut call => break result,
                }
            };
            let llm_stream = match call_result {
                Ok(stream) => {
                    yield AgentEvent::TurnTransition(required_turn_transition(
                        &mut machine,
                        TurnPhase::StreamingModel,
                        TurnTransitionReason::ModelStreamOpened
                    ));
                    stream
                }
                Err(error) => {
                    yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
                    yield AgentEvent::Error(error);
                    return;
                }
            };

            // 5. Consume the stream in a spawned task — events flow
            //    through an mpsc channel into the outer event stream.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
            let (done_tx, done_rx) =
                tokio::sync::oneshot::channel::<Result<ModelResponse, AgentLoopError>>();

            let consumer_task = tokio::spawn(async move {
                let result =
                    consume_provider_stream(llm_stream.stream, llm_stream.expected_model, &tx)
                        .await;
                drop(tx);
                let _ = done_tx.send(result);
            }
            .instrument(tracing::Span::current()));

            // Drain events into the outer stream until consumer ends.
            let stream_err: Option<AgentLoopError> = loop {
                match rx.recv().await {
                    Some(AgentEvent::Error(e)) => break Some(e),
                    Some(ev) => yield ev,
                    None => break None, // consumer finished cleanly
                }
            };

            // Wait for the consumer's final result.
            let Ok(consumer_result) = done_rx.await else {
                yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
                yield AgentEvent::Error(AgentLoopError::Validation(
                    "stream consumer dropped oneshot".into(),
                ));
                return;
            };
            let _ = consumer_task.await;

            if let Some(e) = stream_err {
                yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
                yield AgentEvent::Error(e);
                return;
            }
            let response = match consumer_result {
                Ok(m) => m,
                Err(e) => {
                    yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
                    yield AgentEvent::Error(e);
                    return;
                }
            };
            let response_id = response.id.clone();
            yield AgentEvent::TurnTransition(required_turn_transition(
                &mut machine,
                TurnPhase::FinalizingModelResponse,
                TurnTransitionReason::ModelResponseCompleted
            ));

            let response_stop_reason = response.stop_reason.clone();

            let terminal_response = matches!(
                response_stop_reason,
                StopReason::EndTurn | StopReason::StopSequence(_) | StopReason::Refusal
            );
            yield AgentEvent::ModelResponsePrepared {
                iteration,
                message: assistant_message_from_response(&response),
                terminal: terminal_response,
            };

            // 6. Re-feed assistant message
            machine
                .messages_mut()
                .push(assistant_message_from_response(&response));

            // 7. Execute tools (if any) — events are emitted INSIDE
            //    this iteration's window, before IterationEnd.
            //
            //    Multiple tool_use blocks in one response run in
            //    PARALLEL via futures::join_all. Event ordering is
            //    preserved: all Start events fire first (in tool_use
            //    order), then all End events (in the same order).
            //    This way consumers see a deterministic stream
            //    regardless of which tool finished first.
            let tool_blocks: Vec<PendingToolCall> = response
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => Some(PendingToolCall {
                        id: id.clone(),
                        invocation_id: uuid::Uuid::new_v4().to_string(),
                        name: name.clone(),
                        input: arguments.clone(),
                    }),
                    _ => None,
                })
                .collect();

            if !tool_blocks.is_empty() {
                yield AgentEvent::TurnTransition(required_turn_transition(
                    &mut machine,
                    TurnPhase::PreparingTools,
                    TurnTransitionReason::ToolPreparationStarted
                ));
                let tool_timeout = ports.tool_context.budget.timeout;
                let prepared_calls = tool_blocks
                    .iter()
                    .map(|tool| request.tools.prepare(&tool.name, tool.input.clone()))
                    .collect::<Vec<_>>();
                for (tool, prepared) in tool_blocks.iter().zip(prepared_calls.iter()) {
                    let (invocation_class, recovery_policy, input) = match prepared {
                        Ok(call) => (
                            Some(call.spec().invocation_class),
                            call.spec().recovery_policy,
                            call.input(),
                        ),
                        Err(_) => (
                            None,
                            crate::tool::invocation::ToolRecoveryPolicy::NeverReplay,
                            &tool.input,
                        ),
                    };
                    yield AgentEvent::ToolCallPrepared {
                        id: tool.id.clone(),
                        invocation_id: tool.invocation_id.clone(),
                        name: tool.name.clone(),
                        invocation_class,
                        recovery_policy,
                        input_digest: crate::tool::invocation::prepared_input_digest(input),
                        capability_revision: ports.invocation_snapshot.revision().to_owned(),
                    };
                }

                // Validate and freeze each call before asking for approval.
                // The loop PAUSES here if the gate waits for external input.
                let decisions: Vec<ApprovalDecision> =
                    if let Some(gate) = &ports.approval_gate {
                        yield AgentEvent::TurnTransition(required_turn_transition(
                            &mut machine,
                            TurnPhase::WaitingForApproval,
                            TurnTransitionReason::ApprovalRequired
                        ));
                        // `present_plan` is itself the consent UI. Requiring a
                        // tool approval before showing it would create two
                        // consecutive prompts for one decision.
                        let requests: Vec<ToolUseRequest> = tool_blocks
                            .iter()
                            .zip(prepared_calls.iter())
                            .filter_map(|(tool, prepared)| {
                                (!is_control_tool(&tool.name))
                                    .then(|| prepared.as_ref().ok())
                                    .flatten()
                                    .map(|call| ToolUseRequest::from_prepared(&tool.id, call))
                            })
                            .collect();
                        let mut gated = gate.check_batch(&requests).await.decisions.into_iter();
                        let decisions = tool_blocks
                            .iter()
                            .zip(prepared_calls.iter())
                            .map(|(tool, prepared)| match prepared {
                                Err(error) => ApprovalDecision::Rejected {
                                    reason: error.to_string(),
                                },
                                Ok(_) if is_control_tool(&tool.name) => ApprovalDecision::Approved,
                                Ok(_) => gated.next().unwrap_or_else(|| {
                                        ApprovalDecision::Rejected {
                                            reason: "approval gate returned no decision".into(),
                                        }
                                    }),
                            })
                            .collect();
                        yield AgentEvent::TurnTransition(required_turn_transition(
                            &mut machine,
                            TurnPhase::PreparingTools,
                            TurnTransitionReason::ApprovalResolved
                        ));
                        decisions
                    } else {
                        prepared_calls
                            .iter()
                            .map(|prepared| match prepared {
                                Ok(_) => ApprovalDecision::Approved,
                                Err(error) => ApprovalDecision::Rejected {
                                    reason: error.to_string(),
                                },
                            })
                            .collect()
                    };

                yield AgentEvent::TurnTransition(required_turn_transition(
                    &mut machine,
                    TurnPhase::ExecutingTools,
                    TurnTransitionReason::ToolExecutionStarted
                ));

                let has_control_tool = tool_blocks
                    .iter()
                    .any(|tool| is_control_tool(&tool.name));
                if has_control_tool {
                // Control tools own interactive gates and remain ordered.
                let mut tool_result_blocks = Vec::with_capacity(tool_blocks.len());
                for ((tool_use, decision), prepared_call) in tool_blocks
                    .iter()
                    .zip(decisions.iter())
                    .zip(prepared_calls.iter())
                {
                    match decision {
                        ApprovalDecision::Approved => {
                            yield AgentEvent::ToolCallStart {
                                id: tool_use.id.clone(),
                                name: tool_use.name.clone(),
                                input: tool_use.input.clone(),
                            };

                            // M18: intercept ask_user tool — pause loop, ask user
                            if tool_use.name == "ask_user" {
                                yield AgentEvent::TurnTransition(required_turn_transition(
                                    &mut machine,
                                    TurnPhase::WaitingForUser,
                                    TurnTransitionReason::UserInputRequired
                                ));
                                let question = tool_use.input["question"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string();
                                let options: Vec<String> = tool_use.input["options"]
                                    .as_array()
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|v| v.as_str().map(String::from))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                let multi_select = tool_use.input["multi_select"]
                                    .as_bool()
                                    .unwrap_or(false);

                                yield AgentEvent::AskUser {
                                    call_id: tool_use.id.clone(),
                                    question: question.clone(),
                                    options: options.clone(),
                                    multi_select,
                                };

                                let answer = if let Some(gate) = &ports.ask_user_gate {
                                    gate.ask(
                                        &tool_use.id,
                                        &question,
                                        options.clone(),
                                        multi_select,
                                    )
                                    .await
                                } else {
                                    Vec::new()
                                };
                                yield AgentEvent::TurnTransition(required_turn_transition(
                                    &mut machine,
                                    TurnPhase::ExecutingTools,
                                    TurnTransitionReason::UserInputResolved
                                ));

                                yield AgentEvent::UserAnswer {
                                    call_id: tool_use.id.clone(),
                                    answer: answer.clone(),
                                };

                                yield AgentEvent::ToolCallEnd {
                                    id: tool_use.id.clone(),
                                    name: "ask_user".into(),
                                    output: answer.join(", "),
                                    is_error: false,
                                    failure_kind: None,
                                };
                                tool_result_blocks.push(ContentBlock::tool_result_text(
                                    tool_use.id.clone(),
                                    answer.join(", "),
                                    false,
                                ));
                                continue;
                            }

                            if tool_use.name == "present_plan" {
                                yield AgentEvent::TurnTransition(required_turn_transition(
                                    &mut machine,
                                    TurnPhase::WaitingForPlanReview,
                                    TurnTransitionReason::PlanReviewRequired
                                ));
                                let steps: Vec<String> = tool_use.input["steps"]
                                    .as_array()
                                    .map(|values| {
                                        values
                                            .iter()
                                            .filter_map(|value| value.as_str().map(String::from))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                let plan_id = tool_use.id.clone();

                                yield AgentEvent::PlanProposed {
                                    plan_id: plan_id.clone(),
                                    steps: steps.clone(),
                                };
                                let decision = if let Some(gate) = &ports.plan_gate {
                                    gate.review(&plan_id, steps.clone()).await
                                } else {
                                    PlanDecision::Approved
                                };
                                yield AgentEvent::TurnTransition(required_turn_transition(
                                    &mut machine,
                                    TurnPhase::ExecutingTools,
                                    TurnTransitionReason::PlanReviewResolved
                                ));
                                yield AgentEvent::PlanResolved {
                                    plan_id: plan_id.clone(),
                                    decision: decision.clone(),
                                };

                                let (output, is_error) = match decision {
                                    PlanDecision::Approved => (
                                        "Plan approved. Continue with the proposed steps.".into(),
                                        false,
                                    ),
                                    PlanDecision::Revised { steps } => (
                                        format!(
                                            "Plan revised by the user. Continue with these steps:\n- {}",
                                            steps.join("\n- ")
                                        ),
                                        false,
                                    ),
                                    PlanDecision::Rejected { reason } => (
                                        format!("Plan rejected by the user: {reason}"),
                                        true,
                                    ),
                                };
                                yield AgentEvent::ToolCallEnd {
                                    id: plan_id.clone(),
                                    name: "present_plan".into(),
                                    output: output.clone(),
                                    is_error,
                                    failure_kind: is_error
                                        .then_some(crate::tool::ToolFailureKind::Unclassified),
                                };
                                tool_result_blocks.push(ContentBlock::tool_result_text(
                                    plan_id, output, is_error,
                                ));
                                continue;
                            }

                            if tool_use.name == "start_background_task" {
                                let purpose = tool_use.input["purpose"]
                                    .as_str()
                                    .unwrap_or("Background investigation")
                                    .to_string();
                                let prompt = tool_use.input["prompt"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string();
                                let result = if let Some(gate) = &ports.task_gate {
                                    gate.start(purpose, prompt).await
                                } else {
                                    Err("background task runtime is unavailable".into())
                                };
                                let (output, is_error) = match result {
                                    Ok(task_id) => (
                                        format!("Background task `{task_id}` started."),
                                        false,
                                    ),
                                    Err(error) => (error, true),
                                };
                                yield AgentEvent::ToolCallEnd {
                                    id: tool_use.id.clone(),
                                    name: tool_use.name.clone(),
                                    output: output.clone(),
                                    is_error,
                                    failure_kind: is_error
                                        .then_some(crate::tool::ToolFailureKind::Unclassified),
                                };
                                tool_result_blocks.push(ContentBlock::tool_result_text(
                                    tool_use.id.clone(), output, is_error,
                                ));
                                continue;
                            }

                            if tool_use.name == "update_plan" {
                                let plan_id = tool_use.input["plan_id"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string();
                                let steps = tool_use.input["steps"]
                                    .as_array()
                                    .map(|values| values.iter().filter_map(|value| {
                                        value.as_str().map(String::from)
                                    }).collect::<Vec<_>>())
                                    .unwrap_or_default();
                                let current = tool_use.input["current"]
                                    .as_u64()
                                    .and_then(|value| usize::try_from(value).ok())
                                    .unwrap_or(0)
                                    .min(steps.len().saturating_sub(1));
                                let (output, is_error): (String, bool) =
                                    if plan_id.is_empty() || steps.is_empty() {
                                    ("plan_id and at least one step are required".into(), true)
                                } else if let Some(gate) = &ports.plan_gate {
                                    gate.update(&plan_id, steps, current).await;
                                    ("Visible plan progress updated.".into(), false)
                                } else {
                                    ("plan runtime is unavailable".into(), true)
                                };
                                yield AgentEvent::ToolCallEnd {
                                    id: tool_use.id.clone(),
                                    name: tool_use.name.clone(),
                                    output: output.clone(),
                                    is_error,
                                    failure_kind: is_error
                                        .then_some(crate::tool::ToolFailureKind::Unclassified),
                                };
                                tool_result_blocks.push(ContentBlock::tool_result_text(
                                    tool_use.id.clone(), output, is_error,
                                ));
                                continue;
                            }

                            let name = tool_use.name.clone();
                            let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(
                                crate::tool::TOOL_PROGRESS_CHANNEL_CAPACITY,
                            );
                            let progress_id = tool_use.id.clone();
                            let progress_name = name.clone();
                            let (progress, progress_omission) =
                                crate::tool::ToolProgressSink::bounded(move |delta| {
                                    progress_tx.try_send(delta).is_ok()
                                });
                            let execution = execute_registered_tool(
                                RegisteredToolExecutionRequest {
                                    prepared_call: prepared_call.clone(),
                                    invocation_gateway: ports.invocation_gateway.clone(),
                                    invocation_snapshot: ports.invocation_snapshot.clone(),
                                    tool_context: ports.tool_context.clone(),
                                    call_id: tool_use.id.clone(),
                                    invocation_id: tool_use.invocation_id.clone(),
                                    route: name.clone(),
                                    timeout: tool_timeout,
                                    progress,
                                },
                            );
                            tokio::pin!(execution);
                            let execution = loop {
                                tokio::select! {
                                    biased;
                                    Some(delta) = progress_rx.recv() => {
                                        yield AgentEvent::ToolCallOutputDelta {
                                            id: progress_id.clone(),
                                            name: progress_name.clone(),
                                            delta,
                                        };
                                    }
                                    outcome = &mut execution => break outcome,
                                }
                            };
                            while let Ok(delta) = progress_rx.try_recv() {
                                yield AgentEvent::ToolCallOutputDelta {
                                    id: progress_id.clone(),
                                    name: progress_name.clone(),
                                    delta,
                                };
                            }
                            if progress_omission.occurred() {
                                yield AgentEvent::ToolCallOutputDelta {
                                    id: progress_id,
                                    name: progress_name,
                                    delta: crate::tool::TOOL_PROGRESS_OMITTED_MARKER.into(),
                                };
                            }

                            let ToolExecutionOutcome {
                                output,
                                is_error,
                                timed_out_after,
                                failure_kind,
                            } = execution;
                            if let Some(timeout) = timed_out_after {
                                yield AgentEvent::ToolTimedOut {
                                    id: tool_use.id.clone(),
                                    name: name.clone(),
                                    timeout_secs: timeout.as_secs(),
                                };
                            }
                            yield AgentEvent::ToolCallEnd {
                                id: tool_use.id.clone(),
                                name: name.clone(),
                                output: output.clone(),
                                is_error,
                                failure_kind,
                            };
                            tool_result_blocks.push(ContentBlock::tool_result_text(
                                tool_use.id.clone(), output, is_error,
                            ));
                        }
                        ApprovalDecision::Rejected { reason } => {
                            yield AgentEvent::ToolRejected {
                                id: tool_use.id.clone(),
                                name: tool_use.name.clone(),
                                reason: reason.clone(),
                            };
                            // Re-feed a tool_result with is_error so the model
                            // knows the tool was rejected.
                            tool_result_blocks.push(ContentBlock::tool_result_text(
                                tool_use.id.clone(), reason.clone(), true,
                            ));
                        }
                    }
                }
                machine
                    .messages_mut()
                    .push(ChatMessage::user_blocks(tool_result_blocks));
                } else {
                    // Ordinary tools are independent within one model batch. Emit every
                    // start first, execute concurrently, then publish results in model order.
                    for (tool_use, decision) in tool_blocks.iter().zip(decisions.iter()) {
                        if matches!(decision, ApprovalDecision::Approved) {
                            yield AgentEvent::ToolCallStart {
                                id: tool_use.id.clone(),
                                name: tool_use.name.clone(),
                                input: tool_use.input.clone(),
                            };
                        }
                    }
                    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel(
                        crate::tool::TOOL_PROGRESS_CHANNEL_CAPACITY,
                    );
                    let execution_coordination = Arc::new(tokio::sync::RwLock::new(()));
                    let executions = tool_blocks
                        .iter()
                        .zip(decisions.iter())
                        .zip(prepared_calls.iter())
                        .map(|((tool_use, decision), prepared_call)| {
                        let id = tool_use.id.clone();
                        let invocation_id = tool_use.invocation_id.clone();
                        let name = tool_use.name.clone();
                        let decision = decision.clone();
                        let prepared_call = prepared_call.clone();
                        let execution_mode = prepared_call.as_ref().map_or(
                            crate::tool::ToolExecutionMode::Exclusive,
                            crate::tool::PreparedToolCall::execution_mode,
                        );
                        let execution_coordination = execution_coordination.clone();
                        let invocation_gateway = ports.invocation_gateway.clone();
                        let invocation_snapshot = ports.invocation_snapshot.clone();
                        let context = ports.tool_context.clone();
                        let progress_id = id.clone();
                        let progress_name = name.clone();
                        let progress_tx = progress_tx.clone();
                        let (progress, progress_omission) =
                            crate::tool::ToolProgressSink::bounded(move |delta| {
                                progress_tx
                                    .try_send((
                                        progress_id.clone(),
                                        progress_name.clone(),
                                        delta,
                                    ))
                                    .is_ok()
                            });
                        async move {
                            let outcome = match decision {
                                ApprovalDecision::Approved => {
                                    let execution = match execution_mode {
                                        crate::tool::ToolExecutionMode::Parallel => {
                                            let _guard = execution_coordination.read().await;
                                            execute_registered_tool(
                                                RegisteredToolExecutionRequest {
                                                    prepared_call,
                                                    invocation_gateway,
                                                    invocation_snapshot,
                                                    tool_context: context,
                                                    call_id: id.clone(),
                                                    invocation_id,
                                                    route: name.clone(),
                                                    timeout: tool_timeout,
                                                    progress,
                                                },
                                            ).await
                                        }
                                        crate::tool::ToolExecutionMode::Exclusive => {
                                            let _guard = execution_coordination.write().await;
                                            execute_registered_tool(
                                            RegisteredToolExecutionRequest {
                                                prepared_call,
                                                invocation_gateway,
                                                invocation_snapshot,
                                                tool_context: context,
                                                call_id: id.clone(),
                                                invocation_id,
                                                route: name.clone(),
                                                timeout: tool_timeout,
                                                progress,
                                            },
                                            ).await
                                        }
                                    };
                                    ParallelToolOutcome::Executed(execution)
                                }
                                ApprovalDecision::Rejected { reason } => {
                                    ParallelToolOutcome::Rejected(reason)
                                }
                            };
                            (id, name, outcome, progress_omission.occurred())
                        }
                    });
                    let executions = futures_util::future::join_all(executions);
                    tokio::pin!(executions);
                    let outcomes = loop {
                        tokio::select! {
                            biased;
                            Some((id, name, delta)) = progress_rx.recv() => {
                                yield AgentEvent::ToolCallOutputDelta { id, name, delta };
                            }
                            outcomes = &mut executions => break outcomes,
                        }
                    };
                    while let Ok((id, name, delta)) = progress_rx.try_recv() {
                        yield AgentEvent::ToolCallOutputDelta { id, name, delta };
                    }
                    let mut tool_result_blocks = Vec::with_capacity(outcomes.len());
                    for (id, name, outcome, progress_omitted) in outcomes {
                        if progress_omitted {
                            yield AgentEvent::ToolCallOutputDelta {
                                id: id.clone(),
                                name: name.clone(),
                                delta: crate::tool::TOOL_PROGRESS_OMITTED_MARKER.into(),
                            };
                        }
                        match outcome {
                            ParallelToolOutcome::Executed(execution) => {
                                let ToolExecutionOutcome {
                                    output,
                                    is_error,
                                    timed_out_after,
                                    failure_kind,
                                } = execution;
                                if let Some(timeout) = timed_out_after {
                                    yield AgentEvent::ToolTimedOut {
                                        id: id.clone(),
                                        name: name.clone(),
                                        timeout_secs: timeout.as_secs(),
                                    };
                                }
                                yield AgentEvent::ToolCallEnd {
                                    id: id.clone(),
                                    name,
                                    output: output.clone(),
                                    is_error,
                                    failure_kind,
                                };
                                tool_result_blocks.push(ContentBlock::tool_result_text(
                                    id, output, is_error,
                                ));
                            }
                            ParallelToolOutcome::Rejected(reason) => {
                                yield AgentEvent::ToolRejected {
                                    id: id.clone(),
                                    name,
                                    reason: reason.clone(),
                                };
                                tool_result_blocks.push(ContentBlock::tool_result_text(
                                    id, reason, true,
                                ));
                            }
                        }
                    }
                    machine
                        .messages_mut()
                        .push(ChatMessage::user_blocks(tool_result_blocks));
                }
            }

            let continuation = TurnMachine::continuation_for(&response);
            machine.complete_iteration(response, continuation);

            // 9. Emit IterationEnd — only AFTER all iter-internal
            //    events (chunks + tool calls) have fired.
            yield AgentEvent::IterationEnd {
                iteration,
                response_id,
                usage: machine.cumulative_usage(),
                provider_usage: machine.last_provider_usage(),
            };

            // 10. Check stop_reason.
            //
            //    MaxTokens is NOT terminal — the loop continues so the
            //    model can pick up where it left off. The truncated
            //    assistant message is already in the machine history (re-fed at
            //    step 6), so the next iteration sends the same
            //    conversation and the model continues naturally.
            //
            let terminal = matches!(
                response_stop_reason,
                StopReason::EndTurn
                    | StopReason::StopSequence(_)
                    | StopReason::Refusal
                    | StopReason::Paused
                    | StopReason::Other(_)
            );

            if terminal {
                yield AgentEvent::TurnTransition(required_turn_transition(
                    &mut machine,
                    TurnPhase::RunningAfterHooks,
                    TurnTransitionReason::TerminalModelResponse
                ));
                break;
            }
            let reason = match continuation {
                Some(TurnContinuationReason::MaxOutputTokens) => {
                    TurnTransitionReason::ContinueAfterMaxOutputTokens
                }
                Some(
                    TurnContinuationReason::ToolResultsReady
                    | TurnContinuationReason::ProviderRequestedContinuation,
                )
                | None => TurnTransitionReason::ContinueAfterToolResults,
            };
            yield AgentEvent::TurnTransition(required_turn_transition(
                &mut machine,
                TurnPhase::ReadyForIteration,
                reason,
            ));
        }

        if machine.snapshot().phase == TurnPhase::ReadyForIteration {
            yield AgentEvent::TurnTransition(required_turn_transition(
                &mut machine,
                TurnPhase::RunningAfterHooks,
                TurnTransitionReason::IterationLimitReached
            ));
        }
        if let Err(blocked) = request
            .tools
            .run_turn_hooks(AgentHookPhase::AfterTurn, &ports.tool_context)
            .await
        {
            yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
            yield AgentEvent::Error(AgentLoopError::Tool(blocked.to_string()));
            return;
        }
        let Ok(outcome) = machine.outcome() else {
            yield AgentEvent::TurnTransition(failed_turn_transition(&mut machine));
            yield AgentEvent::Error(AgentLoopError::MaxIterationsReached(
                config.max_iterations,
            ));
            return;
        };
        yield AgentEvent::TurnTransition(required_turn_transition(
            &mut machine,
            TurnPhase::Completed,
            TurnTransitionReason::AfterHooksCompleted
        ));
        yield AgentEvent::Done(outcome);
    }
}

/// Convenience wrapper around [`run_stream`] that consumes the
/// event stream and returns the final [`AgentOutcome`].
///
/// # Errors
/// - [`AgentLoopError::MaxIterationsReached`] — loop hit cap
/// - [`AgentLoopError::Provider`] — qualified provider call failed (after retries)
/// - [`AgentLoopError::Tool`] — non-recoverable tool failure
/// - [`AgentLoopError::IncompatibleModel`] — request requires
///   capability the model doesn't have
pub async fn run(
    config: &AgentLoop,
    request: AgentTurnRequest,
    ports: AgentExecutionPorts,
) -> Result<AgentOutcome, AgentLoopError> {
    let max_iterations = config.max_iterations;
    consume_stream_to_run(max_iterations, run_stream(config, request, ports)).await
}

/// Convenience wrapper around [`run_stream`] that fires every event
/// into the supplied callback, then returns the final [`AgentOutcome`].
/// Terminal `Done` / `Error` events are extracted into the return
/// value rather than fired to the callback.
pub async fn run_with_events<F>(
    config: &AgentLoop,
    request: AgentTurnRequest,
    ports: AgentExecutionPorts,
    mut on_event: F,
) -> Result<AgentOutcome, AgentLoopError>
where
    F: FnMut(AgentEvent) + Send,
{
    let max_iterations = config.max_iterations;
    let mut stream = Box::pin(run_stream(config, request, ports));
    let mut outcome: Option<AgentOutcome> = None;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Done(completed) => outcome = Some(completed),
            AgentEvent::Error(e) => return Err(e),
            other => on_event(other),
        }
    }

    outcome.ok_or(AgentLoopError::MaxIterationsReached(max_iterations))
}

enum ParallelToolOutcome {
    Executed(ToolExecutionOutcome),
    Rejected(String),
}

#[derive(Clone)]
struct PendingToolCall {
    id: String,
    invocation_id: String,
    name: String,
    input: serde_json::Value,
}

fn is_control_tool(name: &str) -> bool {
    matches!(
        name,
        "ask_user" | "present_plan" | "start_background_task" | "update_plan"
    )
}

fn required_turn_transition(
    machine: &mut TurnMachine,
    phase: TurnPhase,
    reason: TurnTransitionReason,
) -> TurnTransition {
    machine
        .transition(phase, reason)
        .unwrap_or_else(|error| panic!("Agent kernel emitted an invalid turn transition: {error}"))
}

fn failed_turn_transition(machine: &mut TurnMachine) -> TurnTransition {
    required_turn_transition(
        machine,
        TurnPhase::Failed,
        TurnTransitionReason::ExecutionFailed,
    )
}

struct ToolExecutionOutcome {
    output: String,
    is_error: bool,
    timed_out_after: Option<std::time::Duration>,
    failure_kind: Option<crate::tool::ToolFailureKind>,
}

/// Exact same-identity tool execution used by a Runtime recovery coordinator.
pub struct RecoveryToolRequest {
    pub tools: crate::tool::ToolRegistry,
    pub invocation_gateway: Arc<dyn crate::tool::invocation::ToolInvocationGateway>,
    pub invocation_snapshot: crate::tool::invocation::ToolInvocationSnapshot,
    pub tool_context: ToolContext,
    pub call_id: String,
    pub invocation_id: String,
    pub route: String,
    pub input: serde_json::Value,
}

/// Model-visible recovered output plus trusted failure classification.
pub struct RecoveryToolOutput {
    pub output: String,
    pub is_error: bool,
    pub failure_kind: Option<crate::tool::ToolFailureKind>,
}

/// Re-enter the unique prepared-tool execution boundary with a stable ID.
pub async fn execute_recovery_tool(request: RecoveryToolRequest) -> RecoveryToolOutput {
    let prepared_call = request.tools.prepare(&request.route, request.input);
    let timeout = request.tool_context.budget.timeout;
    let outcome = execute_registered_tool(RegisteredToolExecutionRequest {
        prepared_call,
        invocation_gateway: request.invocation_gateway,
        invocation_snapshot: request.invocation_snapshot,
        tool_context: request.tool_context,
        call_id: request.call_id,
        invocation_id: request.invocation_id,
        route: request.route,
        timeout,
        progress: crate::tool::ToolProgressSink::new(|_| {}),
    })
    .await;
    RecoveryToolOutput {
        output: outcome.output,
        is_error: outcome.is_error,
        failure_kind: outcome.failure_kind,
    }
}

struct RegisteredToolExecutionRequest {
    prepared_call: Result<crate::tool::PreparedToolCall, crate::tool::ToolPrepareError>,
    invocation_gateway: Arc<dyn crate::tool::invocation::ToolInvocationGateway>,
    invocation_snapshot: crate::tool::invocation::ToolInvocationSnapshot,
    tool_context: ToolContext,
    call_id: String,
    invocation_id: String,
    route: String,
    timeout: Option<std::time::Duration>,
    progress: crate::tool::ToolProgressSink,
}

async fn execute_registered_tool(request: RegisteredToolExecutionRequest) -> ToolExecutionOutcome {
    let RegisteredToolExecutionRequest {
        prepared_call,
        invocation_gateway,
        invocation_snapshot,
        tool_context,
        call_id,
        invocation_id,
        route,
        timeout,
        progress,
    } = request;
    let tool_context = tool_context.with_invocation_call_id(call_id.clone());
    let session_id = tool_context.session_id();
    let trace_id = tool_context.trace_id().unwrap_or("");
    tracing::debug!(%session_id, %trace_id, %call_id, tool = %route, "tool execution started");
    let prepared_call = match prepared_call {
        Ok(call) => call,
        Err(error) => {
            warn!(%session_id, %trace_id, %call_id, tool = %route, %error, "tool preparation failed");
            return ToolExecutionOutcome {
                output: error.to_string(),
                is_error: true,
                timed_out_after: None,
                failure_kind: Some(crate::tool::ToolFailureKind::Unclassified),
            };
        }
    };
    let request = crate::tool::invocation::ToolInvocationRequest::new(
        crate::tool::invocation::ToolInvocationIdentity::new(invocation_id, &call_id),
        &route,
        Some(prepared_call.spec().invocation_class),
        Some(prepared_call.spec().recovery_policy),
        &tool_context,
        prepared_call.input().clone(),
        invocation_snapshot,
    );
    let grant = match invocation_gateway.authorize(request).await {
        Ok(grant) => grant,
        Err(error) => {
            warn!(%session_id, %trace_id, %call_id, tool = %route, %error, "tool authorization failed");
            return ToolExecutionOutcome {
                output: format!("tool authorization failed: {error}"),
                is_error: true,
                timed_out_after: None,
                failure_kind: Some(crate::tool::ToolFailureKind::Unclassified),
            };
        }
    };
    if let Err(error) = prepared_call.validate_environment(&tool_context) {
        warn!(%session_id, %trace_id, %call_id, tool = %route, %error, "tool execution environment rejected");
        let mut outcome = ToolExecutionOutcome {
            output: error.to_string(),
            is_error: true,
            timed_out_after: None,
            failure_kind: Some(crate::tool::ToolFailureKind::Unclassified),
        };
        if let Err(audit_error) = grant
            .finish(crate::tool::invocation::ToolInvocationOutcome::Failed)
            .await
        {
            outcome.output = audit_error.to_string();
        }
        return outcome;
    }
    let (result, timed_out_after) = if let Some(timeout) = timeout {
        if let Ok(result) = tokio::time::timeout(
            timeout,
            prepared_call.execute_streaming(&tool_context, progress),
        )
        .await
        {
            (Some(result), None)
        } else {
            warn!(%session_id, %trace_id, %call_id, tool = %route, "tool execution timed out");
            (None, Some(timeout))
        }
    } else {
        (
            Some(
                prepared_call
                    .execute_streaming(&tool_context, progress)
                    .await,
            ),
            None,
        )
    };
    let (mut outcome, terminal) = match (result, timed_out_after) {
        (None, Some(timeout)) => (
            ToolExecutionOutcome {
                output: format!("tool `{route}` timed out after {}s", timeout.as_secs()),
                is_error: true,
                timed_out_after: Some(timeout),
                failure_kind: Some(crate::tool::ToolFailureKind::Unclassified),
            },
            crate::tool::invocation::ToolInvocationOutcome::TimedOut,
        ),
        (Some(Ok(output)), None) => {
            tracing::debug!(%session_id, %trace_id, %call_id, tool = %route, is_error = output.is_error, "tool execution finished");
            let failure_kind = output.failure_kind();
            let terminal = if output.is_error {
                crate::tool::invocation::ToolInvocationOutcome::Failed
            } else {
                crate::tool::invocation::ToolInvocationOutcome::Succeeded
            };
            (
                ToolExecutionOutcome {
                    output: output.content,
                    is_error: output.is_error,
                    timed_out_after: None,
                    failure_kind,
                },
                terminal,
            )
        }
        (Some(Err(error)), None) => {
            warn!(%session_id, %trace_id, %call_id, tool = %route, %error, "tool execution failed");
            (
                ToolExecutionOutcome {
                    output: format!("tool execution failed: {error}"),
                    is_error: true,
                    timed_out_after: None,
                    failure_kind: Some(crate::tool::ToolFailureKind::Unclassified),
                },
                crate::tool::invocation::ToolInvocationOutcome::Failed,
            )
        }
        _ => unreachable!("timeout and execution result are mutually exclusive"),
    };
    if let Err(error) = grant.finish(terminal).await {
        warn!(%session_id, %trace_id, %call_id, tool = %route, %error, "tool terminal audit failed");
        outcome.output = error.to_string();
        outcome.is_error = true;
        outcome.failure_kind = Some(crate::tool::ToolFailureKind::Unclassified);
    }
    outcome
}

// =====================================================================
// Internal helpers on AgentLoop (private methods used by run_stream)
// =====================================================================

fn provider_retry_cause(error: &sylvander_llm_core::ProviderError) -> ModelRetryCause {
    match error.kind {
        ProviderErrorKind::RateLimited => ModelRetryCause::RateLimit,
        ProviderErrorKind::Unavailable => ModelRetryCause::Server,
        ProviderErrorKind::Transport | ProviderErrorKind::Timeout => ModelRetryCause::Network,
        ProviderErrorKind::Protocol => ModelRetryCause::Stream,
        _ => ModelRetryCause::Other,
    }
}

fn provider_protocol(message: &'static str) -> AgentLoopError {
    AgentLoopError::Provider {
        attempts: 1,
        source: sylvander_llm_core::ProviderError::new(
            sylvander_llm_core::ProviderErrorKind::Protocol,
            sylvander_llm_core::ProviderErrorPhase::Stream,
            message,
        ),
    }
}

async fn consume_provider_stream(
    mut stream: ModelEventStream,
    expected_model: sylvander_llm_core::ModelRef,
    events: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
) -> Result<ModelResponse, AgentLoopError> {
    let mut completed = None;
    while let Some(event) = stream.next().await {
        let event = event.map_err(|source| AgentLoopError::Provider {
            attempts: 1,
            source,
        })?;
        if completed.is_some() {
            return Err(provider_protocol(
                "provider emitted an event after completion",
            ));
        }
        match event {
            sylvander_llm_core::ModelStreamEvent::TextDelta(text) => {
                let _ = events.send(AgentEvent::TextChunk(text));
            }
            sylvander_llm_core::ModelStreamEvent::ReasoningDelta(reasoning) => {
                let _ = events.send(AgentEvent::ThinkingChunk(reasoning));
            }
            sylvander_llm_core::ModelStreamEvent::Completed(response) => {
                if response.model != expected_model {
                    return Err(provider_protocol(
                        "provider completed with an unexpected model",
                    ));
                }
                completed = Some(response);
            }
        }
    }
    let response =
        completed.ok_or_else(|| provider_protocol("provider stream ended without completion"))?;
    Ok(*response)
}

fn auto_compact_llm(
    request: &AgentTurnRequest,
    ports: &AgentExecutionPorts,
) -> crate::context::compression::auto_compact_llm::ProviderAutoCompactLlm {
    crate::context::compression::auto_compact_llm::ProviderAutoCompactLlm::new(
        ports.model.clone(),
        request.model.clone(),
    )
}

impl AgentLoop {
    /// Call the exact qualified router with retry/backoff on transient open
    /// failures. A failed streaming request is never replayed through another
    /// provider or transport.
    async fn call_model_with_retry(
        &self,
        provider_request: ModelRequest,
        retry_events: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        provider: &dyn ModelProvider,
        model: &ModelInfo,
    ) -> Result<LoopModelStream, AgentLoopError> {
        sylvander_llm_core::validate_model_request_capabilities(
            &provider_request,
            model.capabilities,
        )
        .map_err(|error| AgentLoopError::IncompatibleModel(error.to_string()))?;
        let max_attempts = self.max_retries + 1;
        for attempt in 0..max_attempts {
            let result = provider
                .complete_stream(provider_request.clone())
                .await
                .map(|stream| LoopModelStream {
                    stream,
                    expected_model: model.reference.clone(),
                })
                .map_err(|source| AgentLoopError::Provider {
                    attempts: attempt + 1,
                    source,
                });
            match result {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    if !e.is_retryable() || attempt == max_attempts - 1 {
                        return Err(e);
                    }
                    let delay = std::time::Duration::from_millis(100 * (1_u64 << attempt));
                    warn!(
                        attempt = attempt,
                        delay_ms = delay.as_millis(),
                        error = %e,
                        "LLM stream open failed, retrying"
                    );
                    let cause = match &e {
                        AgentLoopError::Provider { source, .. } => provider_retry_cause(source),
                        _ => ModelRetryCause::Other,
                    };
                    let _ = retry_events.send(AgentEvent::ModelRetry {
                        attempt: attempt + 1,
                        max_attempts: self.max_retries,
                        delay_ms: u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                        reason: e.to_string(),
                        cause,
                    });
                    tokio::time::sleep(delay).await;
                }
            }
        }
        unreachable!("retry loop always returns success or the final error")
    }

    fn build_provider_request(
        request: &AgentTurnRequest,
        messages: &[ChatMessage],
    ) -> ModelRequest {
        let tools = tool_definitions_for_model(&request.tools, &request.model);
        ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            model: request.model.reference.clone(),
            system: request.system_instructions.clone(),
            messages: messages.to_vec(),
            tools,
            max_output_tokens: request.model.max_output_tokens,
            reasoning: request.reasoning.map(|mut reasoning| {
                reasoning.budget_tokens = reasoning
                    .budget_tokens
                    .map(|budget| budget.min(request.model.max_output_tokens));
                reasoning
            }),
            output_schema: None,
        }
    }
}

pub(crate) fn tool_definitions_for_model(
    tools: &ToolRegistry,
    model: &ModelInfo,
) -> Vec<sylvander_llm_core::ToolDefinition> {
    let mut definitions = tools.definitions();
    if !model
        .capabilities
        .contains(ModelCapabilities::PROMPT_CACHING)
    {
        for definition in &mut definitions {
            definition.cache_hint = None;
        }
    }
    definitions
}

// =====================================================================
// Free helper (operates on the stream)
// =====================================================================

/// Internal helper for [`run`]: pull events from the stream,
/// return the terminal `AgentOutcome` or the first error.
async fn consume_stream_to_run(
    max_iterations: u32,
    stream: impl Stream<Item = AgentEvent> + Send,
) -> Result<AgentOutcome, AgentLoopError> {
    let mut stream = Box::pin(stream);
    let mut outcome: Option<AgentOutcome> = None;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Done(completed) => {
                outcome = Some(completed);
            }
            AgentEvent::Error(e) => {
                return Err(e);
            }
            _ => {}
        }
    }

    outcome.ok_or(AgentLoopError::MaxIterationsReached(max_iterations))
}

// =====================================================================
// Conversion helpers
// =====================================================================

/// Convert a provider-neutral response into a re-feedable assistant message.
fn assistant_message_from_response(msg: &ModelResponse) -> ChatMessage {
    ChatMessage::assistant(msg.content.clone())
}

// =====================================================================
// Unit tests
// =====================================================================

#[cfg(test)]
#[path = "../../tests/unit/loop_.rs"]
mod tests;
