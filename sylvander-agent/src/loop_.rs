//! `AgentLoop` — the OOP class-based async driver for the agent loop.
//!
//! # Architecture
//!
//! The loop logic lives in three module-level free functions:
//! - [`run`] — consumes the stream, returns `Result<AgentLoopResult, _>`
//! - [`run_stream`] — the single source of truth: drives the
//!   iteration, yields `AgentEvent`s
//! - [`run_with_events`] — consumes the stream, fires events into a
//!   callback, returns the final `AgentLoopResult`
//!
//! `AgentLoop` itself is just a configuration holder (LLM client,
//! model, tools, compressor, iteration limits). The methods
//! `AgentLoop::run`, `AgentLoop::run_stream`, and
//! `AgentLoop::run_with_events` are 1-line delegates to the free
//! functions for callers who prefer method syntax.
//!
//! Adding new event types or consumption patterns only touches
//! `run_stream` — the single iteration implementation.
//!
//! See `projects/Sylvander/designs/sylvander-agent-design.md` for
//! the full design.

use std::sync::Arc;

use futures_util::{Stream, StreamExt};
use tracing::warn;

use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::error::AnthropicError;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{
    ContentBlock, Message, MessageParam, MessageRole, StopReason, ToolResultBlock,
    ToolUseBlock, Usage, UserContentBlock,
};

use super::error::AgentLoopError;
use super::event::AgentEvent;
use super::tool::ToolRegistry;
use super::tool_context::ToolContext;

/// The agent loop. Holds the LLM client, resolved model, tools, and
/// configuration. Iteration logic is in the free functions [`run`],
/// [`run_stream`], and [`run_with_events`].
#[derive(Clone)]
pub struct AgentLoop {
    pub(crate) client: AnthropicClient,
    pub(crate) model: ModelInfo,
    pub(crate) tools: ToolRegistry,
    /// Cached tool definitions for the LLM `tools` field. Built once
    /// at `build()` time and reused every iteration. The registry
    /// is immutable post-build, so this is safe.
    pub(crate) tool_definitions: Vec<sylvander_llm_anthropic::api::types::Tool>,
    pub(crate) compression_pipeline: Arc<super::compress::pipeline::CompressionPipeline>,
    pub(crate) max_iterations: u32,
    pub(crate) max_retries: u32,
    /// Optional system prompt (set via `AgentLoopBuilder::system_prompt`).
    pub(crate) system_prompt: Option<String>,
    /// Optional approval gate — called before tool execution (M12).
    pub(crate) approval_gate: Option<Arc<dyn crate::approval::ApprovalGate>>,
    /// Optional AskUser gate — called for `ask_user` tool (M18).
    pub(crate) ask_user_gate: Option<Arc<dyn crate::ask_user_gate::AskUserGate>>,
    /// Optional plan gate — called for the `present_plan` marker tool.
    pub(crate) plan_gate: Option<Arc<dyn crate::plan_gate::PlanGate>>,
    /// Invocation context handed to every tool call.
    /// Defaults to a placeholder (system user) if the caller doesn't
    /// supply one — keeps tests / examples working unchanged.
    pub(crate) tool_context: ToolContext,
}

impl std::fmt::Debug for AgentLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoop")
            .field("model", &self.model)
            .field("tools", &self.tools)
            .field("max_iterations", &self.max_iterations)
            .field("max_retries", &self.max_retries)
            .finish_non_exhaustive()
    }
}

/// Outcome of a completed [`run`] / [`run_with_events`].
#[derive(Debug, Clone)]
pub struct AgentLoopResult {
    /// Final assembled message (the last assistant turn before the loop
    /// terminated).
    pub final_message: Message,
    /// Total iterations executed.
    pub iterations: u32,
    /// Cumulative token usage across all LLM calls.
    pub total_usage: Usage,
}

// =====================================================================
// Builder
// =====================================================================

/// Builder for [`AgentLoop`].
pub struct AgentLoopBuilder {
    client: Option<AnthropicClient>,
    model: Option<ModelInfo>,
    tools: ToolRegistry,
    compression_pipeline: Option<Arc<super::compress::pipeline::CompressionPipeline>>,
    max_iterations: u32,
    max_retries: u32,
    system_prompt: Option<String>,
    approval_gate: Option<Arc<dyn crate::approval::ApprovalGate>>,
    ask_user_gate: Option<Arc<dyn crate::ask_user_gate::AskUserGate>>,
    plan_gate: Option<Arc<dyn crate::plan_gate::PlanGate>>,
    tool_context: Option<ToolContext>,
}

impl Default for AgentLoopBuilder {
    fn default() -> Self {
        Self {
            client: None,
            model: None,
            tools: ToolRegistry::new(),
            compression_pipeline: None,
            max_iterations: 50,
            max_retries: 3,
            system_prompt: None,
            approval_gate: None,
            ask_user_gate: None,
            plan_gate: None,
            tool_context: None,
        }
    }
}

impl std::fmt::Debug for AgentLoopBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopBuilder")
            .field("client", &self.client.as_ref().map(|_| "AnthropicClient"))
            .field("model", &self.model)
            .field("tools", &self.tools)
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

    /// Set the Anthropic client (required).
    #[must_use]
    pub fn client(mut self, client: AnthropicClient) -> Self {
        self.client = Some(client);
        self
    }

    /// Set the resolved model metadata (required). Caller-built via
    /// `ModelInfo::builder()` — see C11 architecture.
    #[must_use]
    pub fn model(mut self, model: ModelInfo) -> Self {
        self.model = Some(model);
        self
    }

    /// Set the tool registry (replaces any previously set tools).
    #[must_use]
    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Register a single tool (builder-style chaining).
    #[must_use]
    pub fn tool<T: super::tool::Tool + 'static>(mut self, tool: T) -> Self {
        self.tools = self.tools.register(tool);
        self
    }

    /// Set the M3 compression pipeline. If not called, defaults to
    /// [`CompressionPipeline::default_for_model`] (L1 + L2 + L3).
    /// Opt in to L0 or L4 by building a custom pipeline.
    #[must_use]
    pub fn compression_pipeline(
        mut self,
        pipeline: super::compress::pipeline::CompressionPipeline,
    ) -> Self {
        self.compression_pipeline = Some(Arc::new(pipeline));
        self
    }

    /// Set the system prompt. Sent on every LLM request as the
    /// `system` field. If not set, the request omits `system`
    /// (provider default).
    #[must_use]
    pub fn system_prompt(mut self, system: impl Into<String>) -> Self {
        self.system_prompt = Some(system.into());
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

    /// Set the approval gate (M12). If set, the loop calls
    /// [`ApprovalGate::check_batch`](crate::approval::ApprovalGate::check_batch)
    /// before executing each batch of tool calls.
    #[must_use]
    pub fn approval_gate(
        mut self,
        gate: Arc<dyn crate::approval::ApprovalGate>,
    ) -> Self {
        self.approval_gate = Some(gate);
        self
    }

    /// Set the AskUser gate (M18). If set, the loop intercepts
    /// `ask_user` tool calls and routes through the gate.
    #[must_use]
    pub fn ask_user_gate(
        mut self,
        gate: Arc<dyn crate::ask_user_gate::AskUserGate>,
    ) -> Self {
        self.ask_user_gate = Some(gate);
        self
    }

    /// Set the typed plan-review gate. The marker tool is never executed.
    #[must_use]
    pub fn plan_gate(mut self, gate: Arc<dyn crate::plan_gate::PlanGate>) -> Self {
        self.plan_gate = Some(gate);
        self
    }

    /// Build the [`AgentLoop`].
    ///
    /// # Errors
    /// Returns [`AgentLoopError::Builder`] if `client` or `model` is
    /// missing.
    pub fn build(self) -> Result<AgentLoop, AgentLoopError> {
        let client = self
            .client
            .ok_or_else(|| AgentLoopError::Builder("client is required".into()))?;
        let model = self
            .model
            .ok_or_else(|| AgentLoopError::Builder("model is required".into()))?;
        // Default pipeline = L1 + L2 + L3 (cheap, no LLM cost).
        // Opt-in to L0 (disk offload) or L4 (LLM summary) by
        // building a custom pipeline.
        let compression_pipeline = self
            .compression_pipeline
            .unwrap_or_else(|| {
                Arc::new(super::compress::pipeline::CompressionPipeline::default_for_model(&model))
            });

        // Default tool context = system user, agent named after the
        // model id, no session. Production code should call
        // `.tool_context(...)` on the builder; this fallback keeps
        // tests and the M2 quickstart working unchanged.
        let tool_context = self
            .tool_context
            .unwrap_or_else(|| crate::tool_context::defaults::model_tool_context(&model));

        // Cache tool definitions once — tools are immutable post-build.
        let tool_definitions = self.tools.definitions();

        Ok(AgentLoop {
            client,
            model,
            tools: self.tools,
            tool_definitions,
            compression_pipeline,
            max_iterations: self.max_iterations,
            max_retries: self.max_retries,
            system_prompt: self.system_prompt,
            approval_gate: self.approval_gate,
            ask_user_gate: self.ask_user_gate,
            plan_gate: self.plan_gate,
            tool_context,
        })
    }

    /// Set the tool invocation context. If not called, a placeholder
    /// system context is used (see [`build`] for details).
    #[must_use]
    pub fn tool_context(mut self, ctx: ToolContext) -> Self {
        self.tool_context = Some(ctx);
        self
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

    /// Borrow the resolved model metadata.
    #[must_use]
    pub fn model(&self) -> &ModelInfo {
        &self.model
    }

    /// Borrow the tool registry.
    #[must_use]
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
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
/// `config` carries the LLM client, model, tools, compressor, and
/// iteration limits. `initial_messages` seeds the conversation.
///
/// Event order within an iteration:
/// `IterationStart → [Compressed] → [TextChunk* / ThinkingChunk*] →
/// [ToolCallStart → ToolCallEnd]* → IterationEnd → [repeat] → Done | Error`
///
/// On error (capability mismatch, LLM failure after retries,
/// max iterations reached), yields an `AgentEvent::Error(_)` and
/// terminates the stream.
pub fn run_stream(
    config: &AgentLoop,
    initial_messages: Vec<MessageParam>,
) -> impl Stream<Item = AgentEvent> + Send + '_ {
    async_stream::stream! {
        let mut messages = initial_messages;
        let mut total_usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut final_message: Option<Message> = None;

        for iteration in 1..=config.max_iterations {
            yield AgentEvent::IterationStart { iteration };

            // 1. Compression (pipeline: layers run in order, async)
            {
                let auto_llm = super::compress::AgentLoopAutoCompactLlm::new(
                    config.client.clone(),
                );
                let mut compress_ctx = super::compress::CompressContext {
                    messages: &mut messages,
                    last_usage: &total_usage,
                    model_info: &config.model,
                    auto_compact_llm: Some(&auto_llm),
                };
                let reports = config
                    .compression_pipeline
                    .run_all(&mut compress_ctx)
                    .await;
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
                    yield AgentEvent::Compressed { layers: meaningful };
                }
            }

            // 2. Build request
            let request = config.build_request(&messages);

            // 3. Validate capabilities (errors terminate the stream)
            if let Err(e) = config.validate_capabilities(&request) {
                yield AgentEvent::Error(e);
                break;
            }

            // 4. Call LLM with streaming + stream-level retry on transient
            //    errors. If the stream connection drops mid-flight
            //    (5xx, network), we reopen and continue. 4xx / validation
            //    errors still propagate immediately.
            //
            //    The request is the same for each retry — the LLM
            //    generates from the same conversation state, so
            //    reopening is safe.
            const MAX_STREAM_RETRIES: u32 = 2;
            let mut stream_attempt = 0u32;
            let mut llm_stream: Option<sylvander_llm_anthropic::prelude::MessageStream> = None;
            let mut stream_open_err: Option<AgentLoopError> = None;

            loop {
                match config.call_llm_with_retry(&request).await {
                    Ok(s) => {
                        llm_stream = Some(s);
                        break;
                    }
                    Err(AgentLoopError::Llm { source, .. })
                        if source.is_retryable()
                            && stream_attempt < MAX_STREAM_RETRIES =>
                    {
                        stream_attempt += 1;
                        let delay =
                            std::time::Duration::from_millis(100 * (1 << stream_attempt));
                        warn!(
                            stream_attempt,
                            delay_ms = delay.as_millis(),
                            error = %source,
                            "stream open failed, retrying"
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Err(e) => {
                        stream_open_err = Some(e);
                        break;
                    }
                }
            }

            if let Some(e) = stream_open_err {
                yield AgentEvent::Error(e);
                break;
            }

            // 5. Consume the stream in a spawned task — events flow
            //    through an mpsc channel into the outer event stream.
            use futures_util::StreamExt;
            use sylvander_llm_anthropic::api::types::event::ContentDelta;
            use sylvander_llm_anthropic::api::types::RawStreamEvent;

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
            let (done_tx, done_rx) =
                tokio::sync::oneshot::channel::<Result<Message, AgentLoopError>>();

            let mut llm_stream = llm_stream.take().expect("stream must be set after open loop");
            let consumer_task = tokio::spawn(async move {
                let mut stream_err: Option<AgentLoopError> = None;
                while let Some(event_result) = llm_stream.next().await {
                    match event_result {
                        Ok(RawStreamEvent::ContentBlockDelta { delta, .. }) => match delta {
                            ContentDelta::TextDelta { text } => {
                                let _ = tx.send(AgentEvent::TextChunk(text));
                            }
                            ContentDelta::ThinkingDelta { thinking } => {
                                let _ = tx.send(AgentEvent::ThinkingChunk(thinking));
                            }
                            _ => {}
                        },
                        Ok(_) => {} // MessageStart, ContentBlockStart/Stop, etc.
                        Err(e) => {
                            stream_err = Some(AgentLoopError::Llm { retries: 0, source: e });
                            break;
                        }
                    }
                }
                // Drop tx so the receiver sees end-of-stream.
                drop(tx);
                let result = match stream_err {
                    Some(e) => Err(e),
                    None => llm_stream.final_message().ok_or_else(|| {
                        AgentLoopError::Validation("stream ended without final message".into())
                    }),
                };
                let _ = done_tx.send(result);
            });

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
                yield AgentEvent::Error(AgentLoopError::Validation(
                    "stream consumer dropped oneshot".into(),
                ));
                break;
            };
            let _ = consumer_task.await;

            if let Some(e) = stream_err {
                yield AgentEvent::Error(e);
                break;
            }
            let response = match consumer_result {
                Ok(m) => m,
                Err(e) => {
                    yield AgentEvent::Error(e);
                    break;
                }
            };

            let final_message_content = response.content.clone();
            let response_stop_reason = response.stop_reason;
            let response_id = response.id.clone();

            // 6. Re-feed assistant message
            messages.push(assistant_message_from_response(&response));

            // 7. Execute tools (if any) — events are emitted INSIDE
            //    this iteration's window, before IterationEnd.
            //
            //    Multiple tool_use blocks in one response run in
            //    PARALLEL via futures::join_all. Event ordering is
            //    preserved: all Start events fire first (in tool_use
            //    order), then all End events (in the same order).
            //    This way consumers see a deterministic stream
            //    regardless of which tool finished first.
            let tool_blocks: Vec<&ToolUseBlock> = response
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse(t) => Some(t),
                    _ => None,
                })
                .collect();

            if !tool_blocks.is_empty() {
                const TOOL_TIMEOUT: std::time::Duration =
                    std::time::Duration::from_secs(30);

                // Check approval gate before executing tools.
                // The loop PAUSES here if the gate waits for external input.
                let decisions: Vec<crate::approval::ApprovalDecision> =
                    if let Some(gate) = &config.approval_gate {
                        // `present_plan` is itself the consent UI. Requiring a
                        // tool approval before showing it would create two
                        // consecutive prompts for one decision.
                        let requests: Vec<crate::approval::ToolUseRequest> = tool_blocks
                            .iter()
                            .filter(|t| t.name != "present_plan")
                            .map(|t| crate::approval::ToolUseRequest {
                                call_id: t.id.clone(),
                                tool_name: t.name.clone(),
                                input: t.input.clone(),
                            })
                            .collect();
                        let mut gated = gate.check_batch(&requests).await.decisions.into_iter();
                        tool_blocks
                            .iter()
                            .map(|tool| {
                                if tool.name == "present_plan" {
                                    crate::approval::ApprovalDecision::Approved
                                } else {
                                    gated.next().unwrap_or_else(|| {
                                        crate::approval::ApprovalDecision::Rejected {
                                            reason: "approval gate returned no decision".into(),
                                        }
                                    })
                                }
                            })
                            .collect()
                    } else {
                        // No gate → auto-approve all (backward compatible)
                        vec![
                            crate::approval::ApprovalDecision::Approved;
                            tool_blocks.len()
                        ]
                    };

                // Execute approved tools, skip rejected ones
                let mut tool_result_blocks = Vec::with_capacity(tool_blocks.len());
                for (tool_use, decision) in tool_blocks.iter().zip(decisions.iter()) {
                    match decision {
                        crate::approval::ApprovalDecision::Approved => {
                            yield AgentEvent::ToolCallStart {
                                id: tool_use.id.clone(),
                                name: tool_use.name.clone(),
                                input: tool_use.input.clone(),
                            };

                            // M18: intercept ask_user tool — pause loop, ask user
                            if tool_use.name == "ask_user" {
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

                                let answer = if let Some(gate) = &config.ask_user_gate {
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

                                yield AgentEvent::UserAnswer {
                                    call_id: tool_use.id.clone(),
                                    answer: answer.clone(),
                                };

                                yield AgentEvent::ToolCallEnd {
                                    id: tool_use.id.clone(),
                                    name: "ask_user".into(),
                                    output: answer.join(", "),
                                    is_error: false,
                                };
                                tool_result_blocks.push(UserContentBlock::ToolResult(
                                    ToolResultBlock::new(
                                        tool_use.id.clone(),
                                        answer.join(", "),
                                    ),
                                ));
                                continue;
                            }

                            if tool_use.name == "present_plan" {
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
                                let decision = if let Some(gate) = &config.plan_gate {
                                    gate.review(&plan_id, steps.clone()).await
                                } else {
                                    sylvander_protocol::PlanDecision::Approved
                                };
                                yield AgentEvent::PlanResolved {
                                    plan_id: plan_id.clone(),
                                    decision: decision.clone(),
                                };

                                let (output, is_error) = match decision {
                                    sylvander_protocol::PlanDecision::Approved => (
                                        "Plan approved. Continue with the proposed steps.".into(),
                                        false,
                                    ),
                                    sylvander_protocol::PlanDecision::Revised { steps } => (
                                        format!(
                                            "Plan revised by the user. Continue with these steps:\n- {}",
                                            steps.join("\n- ")
                                        ),
                                        false,
                                    ),
                                    sylvander_protocol::PlanDecision::Rejected { reason } => (
                                        format!("Plan rejected by the user: {reason}"),
                                        true,
                                    ),
                                };
                                yield AgentEvent::ToolCallEnd {
                                    id: plan_id.clone(),
                                    name: "present_plan".into(),
                                    output: output.clone(),
                                    is_error,
                                };
                                tool_result_blocks.push(UserContentBlock::ToolResult(
                                    ToolResultBlock::new(plan_id, output).with_error(is_error),
                                ));
                                continue;
                            }

                            let tool = config.tools.get(tool_use.name.as_str());
                            let input = tool_use.input.clone();
                            let name = tool_use.name.clone();
                            let (output, is_error) = if let Some(tool) = tool {
                                match tokio::time::timeout(
                                    TOOL_TIMEOUT,
                                    tool.execute(&config.tool_context, input),
                                )
                                .await
                                {
                                    Ok(Ok(out)) => (out.content, out.is_error),
                                    Ok(Err(e)) => {
                                        warn!(tool = %name, error = %e, "tool execution failed");
                                        (format!("tool execution failed: {e}"), true)
                                    }
                                    Err(_) => {
                                        warn!(tool = %name, "tool execution timed out");
                                        (
                                            format!(
                                                "tool `{}` timed out after {}s",
                                                name,
                                                TOOL_TIMEOUT.as_secs()
                                            ),
                                            true,
                                        )
                                    }
                                }
                            } else {
                                warn!(tool = %name, "tool not found in registry");
                                (format!("tool `{name}` not found in registry"), true)
                            };

                            yield AgentEvent::ToolCallEnd {
                                id: tool_use.id.clone(),
                                name,
                                output: output.clone(),
                                is_error,
                            };
                            tool_result_blocks.push(UserContentBlock::ToolResult(
                                ToolResultBlock::new(tool_use.id.clone(), output)
                                    .with_error(is_error),
                            ));
                        }
                        crate::approval::ApprovalDecision::Rejected { reason } => {
                            yield AgentEvent::ToolRejected {
                                id: tool_use.id.clone(),
                                name: tool_use.name.clone(),
                                reason: reason.clone(),
                            };
                            // Re-feed a tool_result with is_error so the model
                            // knows the tool was rejected.
                            tool_result_blocks.push(UserContentBlock::ToolResult(
                                ToolResultBlock::new(tool_use.id.clone(), reason.clone())
                                    .with_error(true),
                            ));
                        }
                    }
                }
                messages.push(MessageParam::user_blocks(tool_result_blocks));
            }

            // 8. Update running usage (needed for next iteration's
            //    compression trigger checks).
            total_usage = response.usage.clone();

            // 9. Emit IterationEnd — only AFTER all iter-internal
            //    events (chunks + tool calls) have fired.
            yield AgentEvent::IterationEnd {
                iteration,
                usage: total_usage.clone(),
            };

            // 10. Check stop_reason.
            //
            //    MaxTokens is NOT terminal — the loop continues so the
            //    model can pick up where it left off. The truncated
            //    assistant message is already in `messages` (re-fed at
            //    step 6), so the next iteration sends the same
            //    conversation and the model continues naturally.
            //
            //    Always save the latest response as final_message — if
            //    the loop exits without seeing EndTurn (e.g. max_iterations
            //    reached during a MaxTokens chain), the caller sees the
            //    last partial result rather than nothing.
            final_message = Some(Message {
                id: response_id,
                kind: sylvander_llm_anthropic::api::types::MessageKind::Message,
                role: MessageRole::Assistant,
                content: final_message_content,
                model: config.model.id.clone(),
                stop_reason: response_stop_reason,
                stop_sequence: None,
                usage: total_usage.clone(),
            });

            let terminal = matches!(
                response_stop_reason,
                Some(
                    StopReason::EndTurn
                        | StopReason::StopSequence
                        | StopReason::Refusal
                        | StopReason::PauseTurn
                        | StopReason::Other
                )
            );

            if terminal {
                break;
            }
        }

        // Final event: Done or MaxIterationsReached error.
        match final_message {
            Some(msg) => yield AgentEvent::Done(msg),
            None => {
                yield AgentEvent::Error(AgentLoopError::MaxIterationsReached(
                    config.max_iterations,
                ));
            }
        }
    }
}

/// Convenience wrapper around [`run_stream`] that consumes the
/// event stream and returns the final [`AgentLoopResult`].
///
/// # Errors
/// - [`AgentLoopError::MaxIterationsReached`] — loop hit cap
/// - [`AgentLoopError::Llm`] — LLM call failed (after retries)
/// - [`AgentLoopError::Tool`] — non-recoverable tool failure
/// - [`AgentLoopError::IncompatibleModel`] — request requires
///   capability the model doesn't have
pub async fn run(
    config: &AgentLoop,
    initial_messages: Vec<MessageParam>,
) -> Result<AgentLoopResult, AgentLoopError> {
    let max_iterations = config.max_iterations;
    consume_stream_to_run(max_iterations, run_stream(config, initial_messages)).await
}

/// Convenience wrapper around [`run_stream`] that fires every event
/// into the supplied callback, then returns the final [`AgentLoopResult`].
/// Terminal `Done` / `Error` events are extracted into the return
/// value rather than fired to the callback.
pub async fn run_with_events<F>(
    config: &AgentLoop,
    initial_messages: Vec<MessageParam>,
    mut on_event: F,
) -> Result<AgentLoopResult, AgentLoopError>
where
    F: FnMut(AgentEvent) + Send,
{
    let max_iterations = config.max_iterations;
    let mut stream = Box::pin(run_stream(config, initial_messages));
    let mut final_message: Option<Message> = None;
    let mut total_usage = Usage {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    };
    let mut iterations: u32 = 0;

    while let Some(event) = stream.next().await {
        match &event {
            AgentEvent::IterationStart { iteration } => iterations = *iteration,
            AgentEvent::IterationEnd { usage, .. } => total_usage = usage.clone(),
            _ => {}
        }
        match event {
            AgentEvent::Done(msg) => final_message = Some(msg),
            AgentEvent::Error(e) => return Err(e),
            other => on_event(other),
        }
    }

    let final_message = final_message
        .ok_or_else(|| AgentLoopError::MaxIterationsReached(max_iterations))?;

    Ok(AgentLoopResult {
        final_message,
        iterations,
        total_usage,
    })
}

// =====================================================================
// Internal helpers on AgentLoop (private methods used by run_stream)
// =====================================================================

impl AgentLoop {
    /// Call the LLM with retry/backoff on transient errors. Returns
    /// a [`MessageStream`]. Tries streaming first (so `TextChunk`s
    /// arrive as SSE deltas); falls back to non-streaming if the
    /// provider doesn't support SSE.
    async fn call_llm_with_retry(
        &self,
        request: &CreateMessageRequest,
    ) -> Result<sylvander_llm_anthropic::prelude::MessageStream, AgentLoopError> {
        let mut last_err: Option<AnthropicError> = None;
        let max_attempts = self.max_retries + 1;

        // Try streaming first.
        let url = self.client.base_url().join("v1/messages").unwrap();
        tracing::debug!(%url, model=%request.model, max_tokens=request.max_tokens, "calling LLM");
        for attempt in 0..max_attempts {
            match self.client.messages().stream(request).await {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    tracing::warn!(%url, status=?e, "streaming attempt failed");
                    if !e.is_retryable() || attempt == max_attempts - 1 {
                        // Non-retryable (or exhausted retries): try
                        // non-streaming as a fallback. Some providers
                        // (e.g. MiniMax-M3) don't support SSE.
                        warn!(
                            error = %e,
                            "streaming failed, falling back to non-streaming create()"
                        );
                        break;
                    }
                    let delay = std::time::Duration::from_millis(100 * (1 << attempt));
                    warn!(
                        attempt = attempt,
                        delay_ms = delay.as_millis(),
                        error = %e,
                        "LLM stream open failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                }
            }
        }

        // Fallback: non-streaming create(), wrapped as a synthetic
        // MessageStream via from_message().
        for attempt in 0..max_attempts {
            match self.client.messages().create(request).await {
                Ok(msg) => {
                    return Ok(sylvander_llm_anthropic::prelude::MessageStream::from_message(msg));
                }
                Err(e) => {
                    if !e.is_retryable() || attempt == max_attempts - 1 {
                        return Err(AgentLoopError::Llm {
                            retries: attempt,
                            source: e,
                        });
                    }
                    let delay = std::time::Duration::from_millis(100 * (1 << attempt));
                    warn!(
                        attempt = attempt,
                        delay_ms = delay.as_millis(),
                        error = %e,
                        "LLM call failed, retrying"
                    );
                    tokio::time::sleep(delay).await;
                    last_err = Some(e);
                }
            }
        }
        Err(AgentLoopError::Llm {
            retries: self.max_retries,
            source: last_err.expect("retry loop must have errored at least once"),
        })
    }

    /// Validate the request against the model's capabilities.
    fn validate_capabilities(&self, request: &CreateMessageRequest) -> Result<(), AgentLoopError> {
        if !request.tools.is_empty()
            && !self.model.capabilities.contains(ModelCapabilities::TOOL_USE)
        {
            return Err(AgentLoopError::IncompatibleModel(format!(
                "model `{}` does not support TOOL_USE (required because tools are set)",
                self.model.id
            )));
        }

        if request.thinking.is_some()
            && !self
                .model
                .capabilities
                .contains(ModelCapabilities::EXTENDED_THINKING)
        {
            return Err(AgentLoopError::IncompatibleModel(format!(
                "model `{}` does not support EXTENDED_THINKING",
                self.model.id
            )));
        }

        if request.output_config.is_some()
            && !self
                .model
                .capabilities
                .contains(ModelCapabilities::STRUCTURED_OUTPUT)
        {
            return Err(AgentLoopError::IncompatibleModel(format!(
                "model `{}` does not support STRUCTURED_OUTPUT",
                self.model.id
            )));
        }

        Ok(())
    }

    /// Build a `CreateMessageRequest` for the current iteration.
    fn build_request(&self, messages: &[MessageParam]) -> CreateMessageRequest {
        let mut builder = CreateMessageRequest::builder()
            .model(self.model.id.clone())
            .max_tokens(self.model.max_output_tokens)
            .messages(messages.to_vec());

        if let Some(sp) = &self.system_prompt {
            // Use structured Blocks form so we can attach a
            // cache_control breakpoint to the system prompt.
            use sylvander_llm_anthropic::api::types::{
                CacheControl, SystemBlock, SystemPrompt, SystemTextBlock,
            };
            builder = builder.system(SystemPrompt::Blocks(vec![SystemBlock::Text(
                SystemTextBlock::new(sp.clone())
                    .with_cache_control(CacheControl::ephemeral()),
            )]));
        }

        // Use cached tool definitions (built once at construction
        // time; tools are immutable post-build). Avoids re-serializing
        // every iteration.
        if !self.tool_definitions.is_empty() {
            builder = builder.tools(self.tool_definitions.clone());
        }

        builder
            .build()
            .expect("CreateMessageRequest builder fields are pre-validated")
    }
}

// =====================================================================
// Free helper (operates on the stream)
// =====================================================================

/// Internal helper for [`run`]: pull events from the stream,
/// accumulate final state, return `AgentLoopResult` or `Err`.
async fn consume_stream_to_run(
    max_iterations: u32,
    stream: impl Stream<Item = AgentEvent> + Send,
) -> Result<AgentLoopResult, AgentLoopError> {
    let mut stream = Box::pin(stream);
    let mut final_message: Option<Message> = None;
    let mut total_usage = Usage {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    };
    let mut iterations: u32 = 0;

    while let Some(event) = stream.next().await {
        match event {
            AgentEvent::Done(msg) => {
                final_message = Some(msg);
            }
            AgentEvent::Error(e) => {
                return Err(e);
            }
            AgentEvent::IterationStart { iteration } => {
                iterations = iteration;
            }
            AgentEvent::IterationEnd { usage, .. } => {
                total_usage = usage;
            }
            _ => {}
        }
    }

    let final_message = final_message
        .ok_or_else(|| AgentLoopError::MaxIterationsReached(max_iterations))?;
    Ok(AgentLoopResult {
        final_message,
        iterations,
        total_usage,
    })
}

// =====================================================================
// Conversion helpers
// =====================================================================

/// Convert a `Message` response into a `MessageParam` for re-feed.
fn assistant_message_from_response(msg: &Message) -> MessageParam {
    MessageParam::assistant_blocks(msg.content.clone())
}

// Helper trait for ToolResultBlock.with_error() — extend it via
// extension trait since we can't modify upstream.
trait ToolResultExt {
    fn with_error(self, is_error: bool) -> Self;
}

impl ToolResultExt for ToolResultBlock {
    fn with_error(mut self, is_error: bool) -> Self {
        self.is_error = is_error;
        self
    }
}

// =====================================================================
// Unit tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sylvander_llm_anthropic::api::client::AnthropicClient;
    use sylvander_llm_anthropic::api::model::ModelCapabilities;

    fn test_client() -> AnthropicClient {
        AnthropicClient::builder()
            .api_key("test-key")
            .build()
            .expect("client build")
    }

    fn test_model() -> ModelInfo {
        ModelInfo::builder()
            .id("test-model")
            .context_window(200_000)
            .max_output_tokens(8192)
            .capability(ModelCapabilities::TOOL_USE)
            .build()
            .expect("model build")
    }

    #[test]
    fn builder_requires_client() {
        let result = AgentLoop::builder().model(test_model()).build();
        match result {
            Err(AgentLoopError::Builder(msg)) => assert!(msg.contains("client")),
            other => panic!("expected Builder error, got {other:?}"),
        }
    }

    #[test]
    fn builder_requires_model() {
        let result = AgentLoop::builder().client(test_client()).build();
        match result {
            Err(AgentLoopError::Builder(msg)) => assert!(msg.contains("model")),
            other => panic!("expected Builder error, got {other:?}"),
        }
    }

    #[test]
    fn builder_succeeds_with_required_fields() {
        let loop_ = AgentLoop::builder()
            .client(test_client())
            .model(test_model())
            .build()
            .expect("build should succeed");
        assert_eq!(loop_.model().id.as_str(), "test-model");
        assert_eq!(loop_.max_iterations(), 50);
        assert_eq!(loop_.max_retries(), 3);
    }

    #[test]
    fn builder_sets_max_iterations() {
        let loop_ = AgentLoop::builder()
            .client(test_client())
            .model(test_model())
            .max_iterations(10)
            .build()
            .expect("build");
        assert_eq!(loop_.max_iterations(), 10);
    }

    #[test]
    fn builder_sets_max_retries() {
        let loop_ = AgentLoop::builder()
            .client(test_client())
            .model(test_model())
            .max_retries(0)
            .build()
            .expect("build");
        assert_eq!(loop_.max_retries(), 0);
    }

    #[test]
    fn builder_registers_tool() {
        use super::super::tool::MockTool;
        let tool = MockTool::new("echo", "echoes", super::super::tool::ToolOutput::ok("hi"));
        let loop_ = AgentLoop::builder()
            .client(test_client())
            .model(test_model())
            .tool(tool)
            .build()
            .expect("build");
        assert_eq!(loop_.tools().len(), 1);
        assert!(loop_.tools().get("echo").is_some());
    }

    #[test]
    fn default_max_iterations_is_50() {
        let loop_ = AgentLoop::builder()
            .client(test_client())
            .model(test_model())
            .build()
            .expect("build");
        assert_eq!(loop_.max_iterations(), 50);
    }

    #[test]
    fn agent_run_debug_impl() {
        let run = AgentLoopResult {
            final_message: Message {
                id: "msg_x".into(),
                kind: sylvander_llm_anthropic::api::types::MessageKind::Message,
                role: sylvander_llm_anthropic::api::types::MessageRole::Assistant,
                content: vec![],
                model: "test-model".into(),
                stop_reason: Some(sylvander_llm_anthropic::api::types::StopReason::EndTurn),
                stop_sequence: None,
                usage: Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                },
            },
            iterations: 1,
            total_usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };
        let _ = format!("{run:?}");
        let _ = json!({});
    }
}
