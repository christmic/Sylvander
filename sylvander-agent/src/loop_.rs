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
use tracing::{Instrument as _, warn};

use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::error::AnthropicError;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use sylvander_llm_anthropic::api::request::CreateMessageRequest;
use sylvander_llm_anthropic::api::types::{
    ContentBlock, Message, MessageParam, MessageRole, StopReason, ToolResultBlock, ToolUseBlock,
    Usage, UserContentBlock,
};
use sylvander_llm_core::{
    ModelEventStream, ModelInfo as ProviderModelInfo, ModelProvider, ModelRequest,
};
use sylvander_protocol::ModelSelection;

use super::error::AgentLoopError;
use super::event::AgentEvent;
use super::tool::ToolRegistry;
use super::tool_context::ToolContext;

/// The agent loop. Holds the LLM client, resolved model, tools, and
/// configuration. Iteration logic is in the free functions [`run`],
/// [`run_stream`], and [`run_with_events`].
#[derive(Clone)]
pub struct AgentLoop {
    backend: ModelBackend,
    pub(crate) model: ModelInfo,
    pub(crate) reasoning_effort: sylvander_protocol::ReasoningEffort,
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
    /// Optional `AskUser` gate — called for `ask_user` tool (M18).
    pub(crate) ask_user_gate: Option<Arc<dyn crate::ask_user_gate::AskUserGate>>,
    /// Optional plan gate — called for the `present_plan` marker tool.
    pub(crate) plan_gate: Option<Arc<dyn crate::plan_gate::PlanGate>>,
    /// Optional isolated background-task executor.
    pub(crate) task_gate: Option<Arc<dyn crate::task_gate::TaskGate>>,
    /// Invocation context handed to every tool call.
    /// Defaults to a placeholder (system user) if the caller doesn't
    /// supply one — keeps tests / examples working unchanged.
    pub(crate) tool_context: ToolContext,
}

#[derive(Clone)]
enum ModelBackend {
    LegacyAnthropic {
        client: AnthropicClient,
    },
    Provider {
        provider: Arc<dyn ModelProvider>,
        model: ProviderModelInfo,
        routing: ProviderRouting,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderRouting {
    Single,
    Qualified,
}

enum LoopModelStream {
    Legacy(sylvander_llm_anthropic::prelude::MessageStream),
    Provider {
        stream: ModelEventStream,
        expected_model: sylvander_llm_core::ModelRef,
    },
}

impl std::fmt::Debug for AgentLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoop")
            .field("model", &self.model)
            .field("reasoning_effort", &self.reasoning_effort)
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
    provider: Option<Arc<dyn ModelProvider>>,
    provider_model: Option<ProviderModelInfo>,
    provider_routing: Option<ProviderRouting>,
    reasoning_effort: sylvander_protocol::ReasoningEffort,
    tools: ToolRegistry,
    compression_pipeline: Option<Arc<super::compress::pipeline::CompressionPipeline>>,
    max_iterations: u32,
    max_retries: u32,
    system_prompt: Option<String>,
    approval_gate: Option<Arc<dyn crate::approval::ApprovalGate>>,
    ask_user_gate: Option<Arc<dyn crate::ask_user_gate::AskUserGate>>,
    plan_gate: Option<Arc<dyn crate::plan_gate::PlanGate>>,
    task_gate: Option<Arc<dyn crate::task_gate::TaskGate>>,
    tool_context: Option<ToolContext>,
}

impl Default for AgentLoopBuilder {
    fn default() -> Self {
        Self {
            client: None,
            model: None,
            provider: None,
            provider_model: None,
            provider_routing: None,
            reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
            tools: ToolRegistry::new(),
            compression_pipeline: None,
            max_iterations: 50,
            max_retries: 3,
            system_prompt: None,
            approval_gate: None,
            ask_user_gate: None,
            plan_gate: None,
            task_gate: None,
            tool_context: None,
        }
    }
}

impl std::fmt::Debug for AgentLoopBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopBuilder")
            .field("legacy_client_set", &self.client.is_some())
            .field("model", &self.model)
            .field("provider_set", &self.provider.is_some())
            .field("provider_model", &self.provider_model)
            .field("provider_routing", &self.provider_routing)
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

    /// Set a provider-neutral model adapter.
    #[must_use]
    pub fn provider(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.provider = Some(provider);
        self.provider_routing = Some(ProviderRouting::Single);
        self
    }

    /// Set a provider-neutral router that accepts exact qualified models.
    ///
    /// Unlike [`Self::provider`], this explicitly permits a runtime model
    /// selection to cross Provider boundaries. The router remains responsible
    /// for enforcing its immutable qualified allowlist; the loop never falls
    /// back to another route.
    #[must_use]
    pub fn qualified_router(mut self, router: Arc<dyn ModelProvider>) -> Self {
        self.provider = Some(router);
        self.provider_routing = Some(ProviderRouting::Qualified);
        self
    }

    /// Set provider-qualified model metadata.
    #[must_use]
    pub fn provider_model(mut self, model: ProviderModelInfo) -> Self {
        self.provider_model = Some(model);
        self
    }

    #[must_use]
    pub fn reasoning_effort(mut self, effort: sylvander_protocol::ReasoningEffort) -> Self {
        self.reasoning_effort = effort;
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
    pub fn approval_gate(mut self, gate: Arc<dyn crate::approval::ApprovalGate>) -> Self {
        self.approval_gate = Some(gate);
        self
    }

    /// Set the `AskUser` gate (M18). If set, the loop intercepts
    /// `ask_user` tool calls and routes through the gate.
    #[must_use]
    pub fn ask_user_gate(mut self, gate: Arc<dyn crate::ask_user_gate::AskUserGate>) -> Self {
        self.ask_user_gate = Some(gate);
        self
    }

    /// Set the typed plan-review gate. The marker tool is never executed.
    #[must_use]
    pub fn plan_gate(mut self, gate: Arc<dyn crate::plan_gate::PlanGate>) -> Self {
        self.plan_gate = Some(gate);
        self
    }

    #[must_use]
    pub fn task_gate(mut self, gate: Arc<dyn crate::task_gate::TaskGate>) -> Self {
        self.task_gate = Some(gate);
        self
    }

    /// Build the [`AgentLoop`].
    ///
    /// # Errors
    /// Returns [`AgentLoopError::Builder`] when one backend is incomplete,
    /// backends are mixed, or provider execution is not enabled yet.
    pub fn build(self) -> Result<AgentLoop, AgentLoopError> {
        let legacy_set = self.client.is_some() || self.model.is_some();
        let provider_set = self.provider.is_some() || self.provider_model.is_some();
        if legacy_set && provider_set {
            return Err(AgentLoopError::Builder(
                "legacy and provider model backends cannot be mixed".into(),
            ));
        }
        let (backend, model) = if provider_set {
            let provider = self
                .provider
                .ok_or_else(|| AgentLoopError::Builder("provider is required".into()))?;
            let model = self
                .provider_model
                .ok_or_else(|| AgentLoopError::Builder("provider model is required".into()))?;
            let routing = self.provider_routing.ok_or_else(|| {
                AgentLoopError::Builder("provider routing mode is required".into())
            })?;
            let shadow = crate::provider_compat::model_metadata_from_core(&model);
            (
                ModelBackend::Provider {
                    provider,
                    model,
                    routing,
                },
                shadow,
            )
        } else {
            let client = self
                .client
                .ok_or_else(|| AgentLoopError::Builder("client is required".into()))?;
            let model = self
                .model
                .ok_or_else(|| AgentLoopError::Builder("model is required".into()))?;
            (ModelBackend::LegacyAnthropic { client }, model)
        };
        // Default pipeline = L1 + L2 + L3 (cheap, no LLM cost).
        // Opt-in to L0 (disk offload) or L4 (LLM summary) by
        // building a custom pipeline.
        let compression_pipeline = self.compression_pipeline.unwrap_or_else(|| {
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
        // Prompt caching is an optional model feature, so omit its wire hint
        // instead of making otherwise valid tool use incompatible.
        let tool_definitions = tool_definitions_for_model(&self.tools, &model);

        Ok(AgentLoop {
            backend,
            model,
            reasoning_effort: self.reasoning_effort,
            tools: self.tools,
            tool_definitions,
            compression_pipeline,
            max_iterations: self.max_iterations,
            max_retries: self.max_retries,
            system_prompt: self.system_prompt,
            approval_gate: self.approval_gate,
            ask_user_gate: self.ask_user_gate,
            plan_gate: self.plan_gate,
            task_gate: self.task_gate,
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

    #[must_use]
    pub fn reasoning_effort(&self) -> sylvander_protocol::ReasoningEffort {
        self.reasoning_effort
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
        let mut cumulative_usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut last_provider_usage = cumulative_usage.clone();
        let mut final_message: Option<Message> = None;

        for iteration in 1..=config.max_iterations {
            yield AgentEvent::IterationStart { iteration };

            // 1. Compression (pipeline: layers run in order, async)
            {
                let auto_threshold = (config.model.context_window as f32
                    * super::compress::layers::auto_compact::DEFAULT_TRIGGER_RATIO)
                    as u32;
                if last_provider_usage.total_input_tokens() >= auto_threshold && messages.len() > 4 {
                    yield AgentEvent::CompressionStarted;
                }
                let auto_llm = config.auto_compact_llm();
                let mut compress_ctx = super::compress::CompressContext {
                    messages: &mut messages,
                    last_usage: &last_provider_usage,
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
                    yield AgentEvent::Compressed {
                        layers: meaningful.clone(),
                    };
                    yield AgentEvent::HistoryCompacted {
                        history: messages.clone(),
                        layers: meaningful,
                    };
                }
            }

            // 2. Build request
            let request = config.build_request(&messages);

            // 3. Validate capabilities (errors terminate the stream)
            if let Err(e) = config.validate_capabilities(&request) {
                yield AgentEvent::Error(e);
                break;
            }
            let provider_request = match config.build_provider_request(&messages) {
                Ok(request) => request,
                Err(error) => {
                    yield AgentEvent::Error(error);
                    break;
                }
            };

            // 4. Open the provider stream. This is the only retry owner;
            //    provider adapters never retry and a failed streaming
            //    request is never replayed as a buffered request.
            let (retry_tx, mut retry_rx) = tokio::sync::mpsc::unbounded_channel();
            let call = config.call_model_with_retry(&request, provider_request, retry_tx);
            tokio::pin!(call);
            let call_result = loop {
                tokio::select! {
                    biased;
                    Some(retry) = retry_rx.recv() => yield retry,
                    result = &mut call => break result,
                }
            };
            let llm_stream = match call_result {
                Ok(stream) => stream,
                Err(error) => {
                    yield AgentEvent::Error(error);
                    break;
                }
            };

            // 5. Consume the stream in a spawned task — events flow
            //    through an mpsc channel into the outer event stream.
            use futures_util::StreamExt;
            use sylvander_llm_anthropic::api::types::event::ContentDelta;
            use sylvander_llm_anthropic::api::types::RawStreamEvent;

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
            let (done_tx, done_rx) =
                tokio::sync::oneshot::channel::<Result<Message, AgentLoopError>>();

            let consumer_task = tokio::spawn(async move {
                let result = match llm_stream {
                    LoopModelStream::Legacy(mut stream) => {
                        let mut stream_err = None;
                        while let Some(event_result) = stream.next().await {
                            match event_result {
                                Ok(RawStreamEvent::ContentBlockDelta { delta, .. }) => match delta {
                                    ContentDelta::TextDelta { text } => { let _ = tx.send(AgentEvent::TextChunk(text)); }
                                    ContentDelta::ThinkingDelta { thinking } => { let _ = tx.send(AgentEvent::ThinkingChunk(thinking)); }
                                    _ => {}
                                },
                                Ok(_) => {}
                                Err(source) => { stream_err = Some(AgentLoopError::Llm { retries: 0, source }); break; }
                            }
                        }
                        match stream_err {
                            Some(error) => Err(error),
                            None => stream.final_message().ok_or_else(|| AgentLoopError::Validation("stream ended without final message".into())),
                        }
                    }
                    LoopModelStream::Provider { stream, expected_model } => {
                        consume_provider_stream(stream, expected_model, &tx).await
                    }
                };
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
                let tool_timeout = config.tool_context.budget.timeout;

                // Check approval gate before executing tools.
                // The loop PAUSES here if the gate waits for external input.
                let decisions: Vec<crate::approval::ApprovalDecision> =
                    if let Some(gate) = &config.approval_gate {
                        // `present_plan` is itself the consent UI. Requiring a
                        // tool approval before showing it would create two
                        // consecutive prompts for one decision.
                        let requests: Vec<crate::approval::ToolUseRequest> = tool_blocks
                            .iter()
                            .filter(|t| {
                                t.name != "present_plan" && t.name != "start_background_task"
                                    && t.name != "update_plan"
                            })
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
                                if tool.name == "present_plan"
                                    || tool.name == "start_background_task"
                                    || tool.name == "update_plan"
                                {
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

                let has_control_tool = tool_blocks.iter().any(|tool| {
                    matches!(
                        tool.name.as_str(),
                        "ask_user" | "present_plan" | "start_background_task" | "update_plan"
                    )
                });
                if has_control_tool {
                // Control tools own interactive gates and remain ordered.
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

                            if tool_use.name == "start_background_task" {
                                let purpose = tool_use.input["purpose"]
                                    .as_str()
                                    .unwrap_or("Background investigation")
                                    .to_string();
                                let prompt = tool_use.input["prompt"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string();
                                let result = if let Some(gate) = &config.task_gate {
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
                                };
                                tool_result_blocks.push(UserContentBlock::ToolResult(
                                    ToolResultBlock::new(tool_use.id.clone(), output)
                                        .with_error(is_error),
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
                                } else if let Some(gate) = &config.plan_gate {
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
                                };
                                tool_result_blocks.push(UserContentBlock::ToolResult(
                                    ToolResultBlock::new(tool_use.id.clone(), output)
                                        .with_error(is_error),
                                ));
                                continue;
                            }

                            let tool = config.tools.get(tool_use.name.as_str()).cloned();
                            let input = tool_use.input.clone();
                            let name = tool_use.name.clone();
                            let (progress_tx, mut progress_rx) =
                                tokio::sync::mpsc::unbounded_channel();
                            let progress_id = tool_use.id.clone();
                            let progress_name = name.clone();
                            let progress = crate::tool::ToolProgressSink::new(move |delta| {
                                let _ = progress_tx.send(delta);
                            });
                            let execution = execute_registered_tool(
                                tool,
                                &config.tool_context,
                                input,
                                &tool_use.id,
                                &name,
                                tool_timeout,
                                progress,
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

                            let ToolExecutionOutcome {
                                output,
                                is_error,
                                timed_out_after,
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
                } else {
                    // Ordinary tools are independent within one model batch. Emit every
                    // start first, execute concurrently, then publish results in model order.
                    for (tool_use, decision) in tool_blocks.iter().zip(decisions.iter()) {
                        if matches!(decision, crate::approval::ApprovalDecision::Approved) {
                            yield AgentEvent::ToolCallStart {
                                id: tool_use.id.clone(),
                                name: tool_use.name.clone(),
                                input: tool_use.input.clone(),
                            };
                        }
                    }
                    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
                    let executions = tool_blocks.iter().zip(decisions.iter()).map(|(tool_use, decision)| {
                        let id = tool_use.id.clone();
                        let name = tool_use.name.clone();
                        let input = tool_use.input.clone();
                        let decision = decision.clone();
                        let tool = config.tools.get(&name).cloned();
                        let context = config.tool_context.clone();
                        let progress_id = id.clone();
                        let progress_name = name.clone();
                        let progress_tx = progress_tx.clone();
                        let progress = crate::tool::ToolProgressSink::new(move |delta| {
                            let _ = progress_tx.send((
                                progress_id.clone(),
                                progress_name.clone(),
                                delta,
                            ));
                        });
                        async move {
                            let outcome = match decision {
                                crate::approval::ApprovalDecision::Approved => {
                                    ParallelToolOutcome::Executed(
                                        execute_registered_tool(
                                            tool,
                                            &context,
                                            input,
                                            &id,
                                            &name,
                                            tool_timeout,
                                            progress,
                                        ).await,
                                    )
                                }
                                crate::approval::ApprovalDecision::Rejected { reason } => {
                                    ParallelToolOutcome::Rejected(reason)
                                }
                            };
                            (id, name, outcome)
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
                    for (id, name, outcome) in outcomes {
                        match outcome {
                            ParallelToolOutcome::Executed(execution) => {
                                let ToolExecutionOutcome {
                                    output,
                                    is_error,
                                    timed_out_after,
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
                                };
                                tool_result_blocks.push(UserContentBlock::ToolResult(
                                    ToolResultBlock::new(id, output).with_error(is_error),
                                ));
                            }
                            ParallelToolOutcome::Rejected(reason) => {
                                yield AgentEvent::ToolRejected {
                                    id: id.clone(),
                                    name,
                                    reason: reason.clone(),
                                };
                                tool_result_blocks.push(UserContentBlock::ToolResult(
                                    ToolResultBlock::new(id, reason).with_error(true),
                                ));
                            }
                        }
                    }
                    messages.push(MessageParam::user_blocks(tool_result_blocks));
                }
            }

            // 8. Keep the provider's latest context-window report separate
            //    from turn-wide accounting. Compression must not compare a
            //    sum of repeated prompts against one model context window.
            last_provider_usage = response.usage.clone();
            cumulative_usage = saturating_add_usage(&cumulative_usage, &last_provider_usage);

            // 9. Emit IterationEnd — only AFTER all iter-internal
            //    events (chunks + tool calls) have fired.
            yield AgentEvent::IterationEnd {
                iteration,
                usage: cumulative_usage.clone(),
                provider_usage: last_provider_usage.clone(),
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
                usage: cumulative_usage.clone(),
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

    let final_message =
        final_message.ok_or_else(|| AgentLoopError::MaxIterationsReached(max_iterations))?;

    Ok(AgentLoopResult {
        final_message,
        iterations,
        total_usage,
    })
}

enum ParallelToolOutcome {
    Executed(ToolExecutionOutcome),
    Rejected(String),
}

struct ToolExecutionOutcome {
    output: String,
    is_error: bool,
    timed_out_after: Option<std::time::Duration>,
}

async fn execute_registered_tool(
    tool: Option<Arc<dyn crate::tool::Tool>>,
    context: &crate::tool_context::ToolContext,
    input: serde_json::Value,
    call_id: &str,
    name: &str,
    timeout: Option<std::time::Duration>,
    progress: crate::tool::ToolProgressSink,
) -> ToolExecutionOutcome {
    let session_id = &context.session.identity.session_id;
    let trace_id = context.session.request.trace_id.as_deref().unwrap_or("");
    tracing::debug!(%session_id, %trace_id, %call_id, tool = %name, "tool execution started");
    let Some(tool) = tool else {
        warn!(%session_id, %trace_id, %call_id, tool = %name, "tool not found in registry");
        return ToolExecutionOutcome {
            output: format!("tool `{name}` not found in registry"),
            is_error: true,
            timed_out_after: None,
        };
    };
    let result = if let Some(timeout) = timeout {
        if let Ok(result) =
            tokio::time::timeout(timeout, tool.execute_streaming(context, input, progress)).await
        {
            result
        } else {
            warn!(%session_id, %trace_id, %call_id, tool = %name, "tool execution timed out");
            return ToolExecutionOutcome {
                output: format!("tool `{name}` timed out after {}s", timeout.as_secs()),
                is_error: true,
                timed_out_after: Some(timeout),
            };
        }
    } else {
        tool.execute_streaming(context, input, progress).await
    };
    match result {
        Ok(output) => {
            tracing::debug!(%session_id, %trace_id, %call_id, tool = %name, is_error = output.is_error, "tool execution finished");
            ToolExecutionOutcome {
                output: output.content,
                is_error: output.is_error,
                timed_out_after: None,
            }
        }
        Err(error) => {
            warn!(%session_id, %trace_id, %call_id, tool = %name, %error, "tool execution failed");
            ToolExecutionOutcome {
                output: format!("tool execution failed: {error}"),
                is_error: true,
                timed_out_after: None,
            }
        }
    }
}

// =====================================================================
// Internal helpers on AgentLoop (private methods used by run_stream)
// =====================================================================

fn retry_cause(error: &AnthropicError) -> sylvander_protocol::RetryCause {
    match error {
        AnthropicError::Api { status: 429, .. } => sylvander_protocol::RetryCause::RateLimit,
        AnthropicError::Api { status, .. } if *status >= 500 => {
            sylvander_protocol::RetryCause::Server
        }
        AnthropicError::Http(_) => sylvander_protocol::RetryCause::Network,
        AnthropicError::SseParse { .. } => sylvander_protocol::RetryCause::Stream,
        _ => sylvander_protocol::RetryCause::Other,
    }
}

fn provider_retry_cause(
    error: &sylvander_llm_core::ProviderError,
) -> sylvander_protocol::RetryCause {
    use sylvander_llm_core::ProviderErrorKind;
    match error.kind {
        ProviderErrorKind::RateLimited => sylvander_protocol::RetryCause::RateLimit,
        ProviderErrorKind::Unavailable => sylvander_protocol::RetryCause::Server,
        ProviderErrorKind::Transport | ProviderErrorKind::Timeout => {
            sylvander_protocol::RetryCause::Network
        }
        ProviderErrorKind::Protocol => sylvander_protocol::RetryCause::Stream,
        _ => sylvander_protocol::RetryCause::Other,
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
) -> Result<Message, AgentLoopError> {
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
    crate::provider_compat::response_from_core(response)
        .map_err(|error| AgentLoopError::Validation(error.to_string()))
}

impl AgentLoop {
    pub(crate) fn auto_compact_llm(
        &self,
    ) -> super::compress::auto_compact_llm::BackendAutoCompactLlm {
        match &self.backend {
            ModelBackend::LegacyAnthropic { client } => {
                super::compress::auto_compact_llm::BackendAutoCompactLlm::Legacy(
                    super::compress::AgentLoopAutoCompactLlm::new(client.clone()),
                )
            }
            ModelBackend::Provider {
                provider, model, ..
            } => super::compress::auto_compact_llm::BackendAutoCompactLlm::Provider(
                super::compress::auto_compact_llm::ProviderAutoCompactLlm::new(
                    provider.clone(),
                    model.clone(),
                ),
            ),
        }
    }

    /// Apply one exact qualified model to an immutable turn snapshot.
    ///
    /// Single-Provider backends retain their original routing boundary.
    /// Qualified routers may cross that boundary, but only when the selection,
    /// exact provider metadata, and compatibility shadow identify the same
    /// model. Neither mode performs fallback.
    pub(crate) fn apply_runtime_model(
        &mut self,
        selection: &ModelSelection,
        shadow: &ModelInfo,
        exact: Option<&ProviderModelInfo>,
    ) -> Result<(), AgentLoopError> {
        match &mut self.backend {
            ModelBackend::LegacyAnthropic { .. } => {
                if exact.is_some() {
                    return Err(AgentLoopError::IncompatibleModel(
                        "provider metadata cannot be routed by a legacy backend".into(),
                    ));
                }
            }
            ModelBackend::Provider { model, routing, .. } => {
                let exact = exact.ok_or_else(|| {
                    AgentLoopError::IncompatibleModel(
                        "provider-backed model selection lacks exact metadata".into(),
                    )
                })?;
                let exact_matches = exact.reference.provider == selection.provider_id
                    && exact.reference.model == selection.model_id
                    && shadow.id == selection.model_id;
                let route_matches = *routing == ProviderRouting::Qualified
                    || model.reference.provider == selection.provider_id;
                if !exact_matches || !route_matches {
                    return Err(AgentLoopError::IncompatibleModel(format!(
                        "model `{}/{}` is not routed by this Agent",
                        selection.provider_id, selection.model_id
                    )));
                }
                *model = exact.clone();
            }
        }
        self.model = shadow.clone();
        Ok(())
    }

    /// Call the LLM with retry/backoff on transient errors. Returns
    /// a [`MessageStream`]. A provider may normalize a valid buffered
    /// response into a stream, but an error never triggers a second request
    /// using another transport mode.
    async fn call_model_with_retry(
        &self,
        request: &CreateMessageRequest,
        provider_request: Option<ModelRequest>,
        retry_events: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<LoopModelStream, AgentLoopError> {
        match &self.backend {
            ModelBackend::LegacyAnthropic { .. } => self.validate_capabilities(request)?,
            ModelBackend::Provider { model, .. } => {
                let provider_request = provider_request.as_ref().ok_or_else(|| {
                    AgentLoopError::Validation("provider request was not built".into())
                })?;
                sylvander_llm_core::validate_model_request_capabilities(
                    provider_request,
                    model.capabilities,
                )
                .map_err(|error| AgentLoopError::IncompatibleModel(error.to_string()))?;
            }
        }
        let max_attempts = self.max_retries + 1;
        for attempt in 0..max_attempts {
            let result = match &self.backend {
                ModelBackend::LegacyAnthropic { client } => client
                    .messages()
                    .stream(request)
                    .await
                    .map(LoopModelStream::Legacy)
                    .map_err(|source| AgentLoopError::Llm {
                        retries: attempt,
                        source,
                    }),
                ModelBackend::Provider {
                    provider, model, ..
                } => provider
                    .complete_stream(provider_request.clone().ok_or_else(|| {
                        AgentLoopError::Validation("provider request was not built".into())
                    })?)
                    .await
                    .map(|stream| LoopModelStream::Provider {
                        stream,
                        expected_model: model.reference.clone(),
                    })
                    .map_err(|source| AgentLoopError::Provider {
                        attempts: attempt + 1,
                        source,
                    }),
            };
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
                        AgentLoopError::Llm { source, .. } => retry_cause(source),
                        AgentLoopError::Provider { source, .. } => provider_retry_cause(source),
                        _ => sylvander_protocol::RetryCause::Other,
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
        &self,
        messages: &[MessageParam],
    ) -> Result<Option<ModelRequest>, AgentLoopError> {
        let ModelBackend::Provider { model, .. } = &self.backend else {
            return Ok(None);
        };
        let messages = messages
            .iter()
            .map(crate::provider_compat::message_to_core)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| AgentLoopError::Validation(error.to_string()))?;
        let tools = crate::provider_compat::tools_to_core(&self.tool_definitions)
            .map_err(|error| AgentLoopError::Validation(error.to_string()))?;
        Ok(Some(ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            model: model.reference.clone(),
            system: self
                .system_prompt
                .iter()
                .map(|text| sylvander_llm_core::SystemInstruction {
                    text: text.clone(),
                    cache_hint: self
                        .model
                        .capabilities
                        .contains(ModelCapabilities::PROMPT_CACHING)
                        .then_some(sylvander_llm_core::CacheHint::Ephemeral),
                })
                .collect(),
            messages,
            tools,
            max_output_tokens: model.max_output_tokens,
            reasoning: self.reasoning_effort.budget_tokens().map(|budget_tokens| {
                sylvander_llm_core::ReasoningConfig {
                    budget_tokens: budget_tokens.min(model.max_output_tokens),
                }
            }),
            output_schema: None,
        }))
    }

    /// Validate the request against the model's capabilities.
    fn validate_capabilities(&self, request: &CreateMessageRequest) -> Result<(), AgentLoopError> {
        use sylvander_llm_anthropic::api::types::{
            SystemBlock, SystemPrompt, UserContent, UserContentBlock,
        };

        let mut history_tool = false;
        let mut history_thinking = false;
        let mut image = false;
        for message in &request.messages {
            let UserContent::Blocks(blocks) = &message.content else {
                continue;
            };
            for block in blocks {
                match block {
                    UserContentBlock::ToolResult(_) => history_tool = true,
                    UserContentBlock::Image(_) => image = true,
                    UserContentBlock::Other(value) => {
                        match value.get("type").and_then(serde_json::Value::as_str) {
                            Some("tool_use") => history_tool = true,
                            Some("thinking") => history_thinking = true,
                            _ => {}
                        }
                    }
                    UserContentBlock::Text(_) => {}
                }
            }
        }
        let cached_system = matches!(
            &request.system,
            Some(SystemPrompt::Blocks(blocks))
                if blocks.iter().any(|block| matches!(
                    block,
                    SystemBlock::Text(text) if text.cache_control.is_some()
                ))
        );
        let cached_tool = request
            .tools
            .iter()
            .any(|tool| tool.cache_control.is_some());

        if (!request.tools.is_empty() || history_tool)
            && !self
                .model
                .capabilities
                .contains(ModelCapabilities::TOOL_USE)
        {
            return Err(AgentLoopError::IncompatibleModel(
                "legacy request requires unsupported TOOL_USE capability".into(),
            ));
        }

        if (request.thinking.is_some() || history_thinking)
            && !self
                .model
                .capabilities
                .contains(ModelCapabilities::EXTENDED_THINKING)
        {
            return Err(AgentLoopError::IncompatibleModel(
                "legacy request requires unsupported EXTENDED_THINKING capability".into(),
            ));
        }

        if request.output_config.is_some()
            && !self
                .model
                .capabilities
                .contains(ModelCapabilities::STRUCTURED_OUTPUT)
        {
            return Err(AgentLoopError::IncompatibleModel(
                "legacy request requires unsupported STRUCTURED_OUTPUT capability".into(),
            ));
        }

        if (cached_system || cached_tool)
            && !self
                .model
                .capabilities
                .contains(ModelCapabilities::PROMPT_CACHING)
        {
            return Err(AgentLoopError::IncompatibleModel(
                "legacy request requires unsupported PROMPT_CACHING capability".into(),
            ));
        }

        if image && !self.model.capabilities.contains(ModelCapabilities::VISION) {
            return Err(AgentLoopError::IncompatibleModel(
                "legacy request requires unsupported VISION capability".into(),
            ));
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
            use sylvander_llm_anthropic::api::types::{SystemBlock, SystemPrompt, SystemTextBlock};
            let mut block = SystemTextBlock::new(sp.clone());
            if self
                .model
                .capabilities
                .contains(ModelCapabilities::PROMPT_CACHING)
            {
                use sylvander_llm_anthropic::api::types::CacheControl;
                block = block.with_cache_control(CacheControl::ephemeral());
            }
            builder = builder.system(SystemPrompt::Blocks(vec![SystemBlock::Text(block)]));
        }

        if let Some(budget) = self.reasoning_effort.budget_tokens() {
            builder = builder.thinking(budget.min(self.model.max_output_tokens));
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

pub(crate) fn tool_definitions_for_model(
    tools: &ToolRegistry,
    model: &ModelInfo,
) -> Vec<sylvander_llm_anthropic::api::types::Tool> {
    let mut definitions = tools.definitions();
    if !model
        .capabilities
        .contains(ModelCapabilities::PROMPT_CACHING)
    {
        for definition in &mut definitions {
            definition.cache_control = None;
        }
    }
    definitions
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

    let final_message =
        final_message.ok_or_else(|| AgentLoopError::MaxIterationsReached(max_iterations))?;
    Ok(AgentLoopResult {
        final_message,
        iterations,
        total_usage,
    })
}

fn saturating_add_usage(total: &Usage, next: &Usage) -> Usage {
    Usage {
        input_tokens: total.input_tokens.saturating_add(next.input_tokens),
        output_tokens: total.output_tokens.saturating_add(next.output_tokens),
        cache_creation_input_tokens: saturating_add_optional_tokens(
            total.cache_creation_input_tokens,
            next.cache_creation_input_tokens,
        ),
        cache_read_input_tokens: saturating_add_optional_tokens(
            total.cache_read_input_tokens,
            next.cache_read_input_tokens,
        ),
    }
}

fn saturating_add_optional_tokens(total: Option<u32>, next: Option<u32>) -> Option<u32> {
    match (total, next) {
        (None, None) => None,
        (total, next) => Some(total.unwrap_or(0).saturating_add(next.unwrap_or(0))),
    }
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
    use sylvander_llm_core::{
        CacheHint, ChatMessage, ChatRole, ContentBlock as ProviderBlock, DocumentContent,
        ImageContent, MediaSource, ModelCapabilities as ProviderCapabilities, ModelEventStream,
        ModelRef, ModelResponse, ModelStreamEvent, ProviderError, ProviderErrorKind,
        ProviderErrorPhase, ProviderFuture, StopReason as ProviderStopReason, SystemInstruction,
        TokenUsage, ToolResultContent,
    };

    type ProviderOpen = Result<Vec<Result<ModelStreamEvent, ProviderError>>, ProviderError>;

    struct ScriptedProvider {
        opens: std::sync::Mutex<std::collections::VecDeque<ProviderOpen>>,
        requests: std::sync::Mutex<Vec<ModelRequest>>,
    }

    impl ScriptedProvider {
        fn new(opens: impl IntoIterator<Item = ProviderOpen>) -> Self {
            Self {
                opens: std::sync::Mutex::new(opens.into_iter().collect()),
                requests: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl ModelProvider for ScriptedProvider {
        fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
            self.requests.lock().unwrap().push(request);
            let open = self.opens.lock().unwrap().pop_front().unwrap();
            Box::pin(async move {
                open.map(|events| Box::pin(futures_util::stream::iter(events)) as ModelEventStream)
            })
        }
    }

    struct FakeProvider {
        _secret: &'static str,
    }

    impl ModelProvider for FakeProvider {
        fn complete_stream(
            &self,
            _request: sylvander_llm_core::ModelRequest,
        ) -> ProviderFuture<'_> {
            Box::pin(async {
                let stream: ModelEventStream = Box::pin(futures_util::stream::empty());
                Ok(stream)
            })
        }
    }

    struct SlowTool;

    #[async_trait::async_trait]
    impl crate::tool::Tool for SlowTool {
        fn name(&self) -> &'static str {
            "slow"
        }

        fn description(&self) -> &'static str {
            "waits beyond its deadline"
        }

        fn input_schema(&self) -> sylvander_llm_anthropic::api::types::InputSchema {
            sylvander_llm_anthropic::api::types::InputSchema::empty()
        }

        async fn execute(
            &self,
            _ctx: &crate::tool_context::ToolContext,
            _input: serde_json::Value,
        ) -> Result<crate::tool::ToolOutput, crate::tool::ToolError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn tool_deadline_is_a_typed_outcome() {
        let outcome = execute_registered_tool(
            Some(Arc::new(SlowTool)),
            &crate::tool_context::defaults::system_tool_context(),
            serde_json::json!({}),
            "call-slow",
            "slow",
            Some(std::time::Duration::from_millis(1)),
            crate::tool::ToolProgressSink::new(|_| {}),
        )
        .await;
        assert_eq!(
            outcome.timed_out_after,
            Some(std::time::Duration::from_millis(1))
        );
        assert!(outcome.is_error);
        assert!(outcome.output.contains("timed out"));
    }

    fn test_client() -> AnthropicClient {
        AnthropicClient::builder()
            .api_key("test-key")
            .build()
            .expect("client build")
    }

    fn test_model() -> ModelInfo {
        shadow_model("test-model")
    }

    fn shadow_model(model_id: &str) -> ModelInfo {
        ModelInfo::builder()
            .id(model_id)
            .context_window(200_000)
            .max_output_tokens(8192)
            .capability(ModelCapabilities::TOOL_USE)
            .build()
            .expect("model build")
    }

    fn provider_model() -> ProviderModelInfo {
        provider_model_for("local", "test-model")
    }

    fn provider_model_for(provider_id: &str, model_id: &str) -> ProviderModelInfo {
        ProviderModelInfo {
            reference: ModelRef::new(provider_id, model_id),
            context_window: 100_000,
            max_output_tokens: 4096,
            capabilities: ProviderCapabilities::TOOL_USE,
        }
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
    fn provider_builder_preserves_qualified_identity_and_safe_debug() {
        let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider {
            _secret: "secret-provider-state",
        });
        let builder = AgentLoop::builder()
            .provider(provider)
            .provider_model(provider_model());
        let debug = format!("{builder:?}");
        assert!(!debug.contains("secret-provider-state"));
        let loop_ = builder.build().unwrap();
        assert_eq!(loop_.model.id, "test-model");
        assert!(matches!(
            &loop_.backend,
            ModelBackend::Provider { model, routing, .. }
                if model.reference == ModelRef::new("local", "test-model")
                    && *routing == ProviderRouting::Single
        ));
    }

    #[test]
    fn prompt_cache_hints_follow_the_selected_model_capability() {
        for enabled in [false, true] {
            let capabilities = if enabled {
                ProviderCapabilities::TOOL_USE | ProviderCapabilities::PROMPT_CACHING
            } else {
                ProviderCapabilities::TOOL_USE
            };
            let model = ProviderModelInfo {
                reference: ModelRef::new("local", "cache-model"),
                context_window: 100_000,
                max_output_tokens: 4096,
                capabilities,
            };
            let loop_ = AgentLoop::builder()
                .provider(Arc::new(FakeProvider {
                    _secret: "not-resolved",
                }))
                .provider_model(model)
                .system_prompt("stable instructions")
                .tool(crate::tool::MockTool::new(
                    "read",
                    "read a file",
                    crate::tool::ToolOutput::ok("done"),
                ))
                .build()
                .unwrap();

            assert_eq!(loop_.tool_definitions[0].cache_control.is_some(), enabled);
            let legacy =
                serde_json::to_value(loop_.build_request(&[MessageParam::user("go")])).unwrap();
            assert_eq!(legacy.pointer("/system/0/cache_control").is_some(), enabled);
            let neutral = loop_
                .build_provider_request(&[MessageParam::user("go")])
                .unwrap()
                .unwrap();
            assert_eq!(neutral.system[0].cache_hint.is_some(), enabled);
            assert_eq!(neutral.tools[0].cache_hint.is_some(), enabled);
        }
    }

    #[tokio::test]
    async fn legacy_history_media_and_cache_fail_before_dispatch() {
        use sylvander_llm_anthropic::api::types::{
            CacheControl, ImageBlock, SystemBlock, SystemPrompt, SystemTextBlock, ThinkingBlock,
            UserContentBlock,
        };
        use wiremock::MockServer;

        let server = MockServer::start().await;
        let client = AnthropicClient::builder()
            .api_key("test-key")
            .base_url(server.uri())
            .build()
            .unwrap();
        let model = ModelInfo::builder()
            .id("legacy-model")
            .context_window(100_000)
            .max_output_tokens(4096)
            .build()
            .unwrap();
        let loop_ = AgentLoop::builder()
            .client(client)
            .model(model)
            .max_retries(0)
            .build()
            .unwrap();
        let tool_call =
            loop_.build_request(&[MessageParam::assistant_blocks(vec![ContentBlock::ToolUse(
                ToolUseBlock::new("secret-call", "secret-tool", json!({"secret": true})),
            )])]);
        let tool_result = loop_.build_request(&[MessageParam::user_blocks(vec![
            UserContentBlock::ToolResult(ToolResultBlock::new("secret-call", "secret-result")),
        ])]);
        let thinking = loop_.build_request(&[MessageParam::assistant_blocks(vec![
            ContentBlock::Thinking(ThinkingBlock::new("secret-thinking", "secret-signature")),
        ])]);
        let image =
            loop_.build_request(&[MessageParam::user_blocks(vec![UserContentBlock::Image(
                ImageBlock::png("secret-image"),
            )])]);
        let mut cache = loop_.build_request(&[MessageParam::user("hello")]);
        cache.system = Some(SystemPrompt::Blocks(vec![SystemBlock::Text(
            SystemTextBlock::new("secret-system").with_cache_control(CacheControl::ephemeral()),
        )]));

        for request in [tool_call, tool_result, thinking, image, cache] {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let Err(error) = loop_.call_model_with_retry(&request, None, tx).await else {
                panic!("unsupported legacy request reached dispatch");
            };
            assert!(matches!(error, AgentLoopError::IncompatibleModel(_)));
            assert!(!error.is_retryable());
            assert!(!error.to_string().contains("secret"));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn single_provider_rejects_cross_provider_runtime_model() {
        let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider { _secret: "secret" });
        let mut loop_ = AgentLoop::builder()
            .provider(provider)
            .provider_model(provider_model())
            .build()
            .unwrap();
        let selection = ModelSelection {
            provider_id: "remote".into(),
            model_id: "model-b".into(),
        };
        let error = loop_
            .apply_runtime_model(
                &selection,
                &shadow_model("model-b"),
                Some(&provider_model_for("remote", "model-b")),
            )
            .unwrap_err();
        assert!(matches!(error, AgentLoopError::IncompatibleModel(_)));
        assert!(matches!(
            &loop_.backend,
            ModelBackend::Provider { model, routing, .. }
                if model.reference == ModelRef::new("local", "test-model")
                    && *routing == ProviderRouting::Single
        ));
    }

    #[test]
    fn qualified_router_accepts_cross_provider_runtime_model() {
        let router: Arc<dyn ModelProvider> = Arc::new(FakeProvider { _secret: "secret" });
        let mut loop_ = AgentLoop::builder()
            .qualified_router(router)
            .provider_model(provider_model())
            .build()
            .unwrap();
        let selection = ModelSelection {
            provider_id: "remote".into(),
            model_id: "model-b".into(),
        };
        loop_
            .apply_runtime_model(
                &selection,
                &shadow_model("model-b"),
                Some(&provider_model_for("remote", "model-b")),
            )
            .unwrap();
        assert_eq!(loop_.model.id, "model-b");
        assert!(matches!(
            &loop_.backend,
            ModelBackend::Provider { model, routing, .. }
                if model.reference == ModelRef::new("remote", "model-b")
                    && *routing == ProviderRouting::Qualified
        ));
    }

    #[test]
    fn qualified_router_rejects_any_runtime_identity_mismatch() {
        let router: Arc<dyn ModelProvider> = Arc::new(FakeProvider { _secret: "secret" });
        let mut loop_ = AgentLoop::builder()
            .qualified_router(router)
            .provider_model(provider_model())
            .build()
            .unwrap();
        let selection = ModelSelection {
            provider_id: "remote".into(),
            model_id: "model-b".into(),
        };
        let cases = [
            (
                shadow_model("model-b"),
                provider_model_for("remote", "wrong"),
            ),
            (
                shadow_model("wrong"),
                provider_model_for("remote", "model-b"),
            ),
            (
                shadow_model("model-b"),
                provider_model_for("wrong", "model-b"),
            ),
        ];
        for (shadow, exact) in cases {
            assert!(matches!(
                loop_.apply_runtime_model(&selection, &shadow, Some(&exact)),
                Err(AgentLoopError::IncompatibleModel(_))
            ));
        }
        assert!(matches!(
            &loop_.backend,
            ModelBackend::Provider { model, .. }
                if model.reference == ModelRef::new("local", "test-model")
        ));
    }

    fn completed_events(
        content: Vec<ProviderBlock>,
        stop_reason: ProviderStopReason,
    ) -> Vec<Result<ModelStreamEvent, ProviderError>> {
        vec![Ok(ModelStreamEvent::Completed(ModelResponse {
            id: "response".into(),
            model: ModelRef::new("local", "test-model"),
            content,
            stop_reason,
            usage: TokenUsage::default(),
        }))]
    }

    fn neutral_request() -> ModelRequest {
        ModelRequest {
            request_id: "secret-request".into(),
            model: ModelRef::new("local", "test-model"),
            system: Vec::new(),
            messages: vec![ChatMessage::user("hello")],
            tools: Vec::new(),
            max_output_tokens: 100,
            reasoning: None,
            output_schema: None,
        }
    }

    fn neutral_image() -> ImageContent {
        ImageContent {
            source: MediaSource::Url {
                url: "https://secret.invalid/image".into(),
            },
            alt_text: None,
        }
    }

    fn neutral_document() -> DocumentContent {
        DocumentContent {
            source: MediaSource::Url {
                url: "https://secret.invalid/document".into(),
            },
            title: Some("secret-document".into()),
        }
    }

    fn provider_loop_with_capabilities(
        provider: Arc<ScriptedProvider>,
        capabilities: ProviderCapabilities,
    ) -> AgentLoop {
        AgentLoop::builder()
            .provider(provider)
            .provider_model(ProviderModelInfo {
                reference: ModelRef::new("local", "test-model"),
                context_window: 100_000,
                max_output_tokens: 4096,
                capabilities,
            })
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn provider_capability_preflight_rejects_before_dispatch() {
        let mut tool_call = neutral_request();
        tool_call.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: vec![ProviderBlock::ToolCall {
                id: "secret-call".into(),
                name: "secret-tool".into(),
                arguments: json!({"secret": true}),
            }],
        });
        let mut tool_result = neutral_request();
        tool_result.messages.push(ChatMessage {
            role: ChatRole::User,
            content: vec![ProviderBlock::ToolResult {
                call_id: "secret-call".into(),
                content: vec![ToolResultContent::Text {
                    text: "secret-result".into(),
                }],
                is_error: false,
            }],
        });
        let mut reasoning = neutral_request();
        reasoning.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: vec![ProviderBlock::Reasoning {
                text: "secret-reasoning".into(),
                opaque_state: None,
            }],
        });
        let mut image = neutral_request();
        image.messages.push(ChatMessage {
            role: ChatRole::User,
            content: vec![ProviderBlock::Image {
                image: neutral_image(),
            }],
        });
        let mut document = neutral_request();
        document.messages.push(ChatMessage {
            role: ChatRole::User,
            content: vec![ProviderBlock::Document {
                document: neutral_document(),
            }],
        });
        let mut schema = neutral_request();
        schema.output_schema = Some(json!({"secret-schema": true}));
        let mut cache = neutral_request();
        cache.system.push(SystemInstruction {
            text: "secret-system".into(),
            cache_hint: Some(CacheHint::Ephemeral),
        });

        let provider = Arc::new(ScriptedProvider::new(Vec::<ProviderOpen>::new()));
        let loop_ =
            provider_loop_with_capabilities(provider.clone(), ProviderCapabilities::empty());
        let legacy = loop_.build_request(&[MessageParam::user("legacy-placeholder")]);
        for request in [
            tool_call,
            tool_result,
            reasoning,
            image,
            document,
            schema,
            cache,
        ] {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let Err(error) = loop_
                .call_model_with_retry(&legacy, Some(request), tx)
                .await
            else {
                panic!("unsupported request reached provider dispatch");
            };
            assert!(matches!(error, AgentLoopError::IncompatibleModel(_)));
            assert!(!error.is_retryable());
            assert!(!error.to_string().contains("secret"));
        }
        assert!(provider.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn provider_capability_preflight_dispatches_once_when_fully_supported() {
        let provider = Arc::new(ScriptedProvider::new([Ok(completed_events(
            vec![ProviderBlock::Text { text: "ok".into() }],
            ProviderStopReason::EndTurn,
        ))]));
        let all = ProviderCapabilities::TOOL_USE
            | ProviderCapabilities::REASONING
            | ProviderCapabilities::STRUCTURED_OUTPUT
            | ProviderCapabilities::PROMPT_CACHING
            | ProviderCapabilities::VISION
            | ProviderCapabilities::DOCUMENT_INPUT;
        let loop_ = provider_loop_with_capabilities(provider.clone(), all);
        let legacy = loop_.build_request(&[MessageParam::user("legacy-placeholder")]);
        let mut request = neutral_request();
        request.output_schema = Some(json!({"type": "object"}));
        request.system.push(SystemInstruction {
            text: "system".into(),
            cache_hint: Some(CacheHint::Ephemeral),
        });
        request.reasoning = Some(sylvander_llm_core::ReasoningConfig { budget_tokens: 10 });
        request.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: vec![
                ProviderBlock::Reasoning {
                    text: "reasoning".into(),
                    opaque_state: None,
                },
                ProviderBlock::ToolCall {
                    id: "call".into(),
                    name: "tool".into(),
                    arguments: json!({}),
                },
            ],
        });
        request.messages.push(ChatMessage {
            role: ChatRole::User,
            content: vec![ProviderBlock::ToolResult {
                call_id: "call".into(),
                content: vec![
                    ToolResultContent::Image {
                        image: neutral_image(),
                    },
                    ToolResultContent::Document {
                        document: neutral_document(),
                    },
                ],
                is_error: false,
            }],
        });
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        loop_
            .call_model_with_retry(&legacy, Some(request), tx)
            .await
            .unwrap();
        assert_eq!(provider.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn provider_backend_runs_tool_then_text_with_qualified_requests() {
        let provider = Arc::new(ScriptedProvider::new([
            Ok(completed_events(
                vec![ProviderBlock::ToolCall {
                    id: "call-1".into(),
                    name: "echo".into(),
                    arguments: json!({"value": 7}),
                }],
                ProviderStopReason::ToolUse,
            )),
            Ok(completed_events(
                vec![ProviderBlock::Text {
                    text: "done".into(),
                }],
                ProviderStopReason::EndTurn,
            )),
        ]));
        let tool =
            crate::tool::MockTool::new("echo", "echo input", crate::tool::ToolOutput::ok("7"));
        let loop_ = AgentLoop::builder()
            .provider(provider.clone())
            .provider_model(provider_model())
            .tool(tool.clone())
            .build()
            .unwrap();
        let result = run(&loop_, vec![MessageParam::user("start")])
            .await
            .unwrap();
        assert_eq!(result.iterations, 2);
        assert_eq!(tool.call_count(), 1);
        let requests = provider.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(
            requests
                .iter()
                .all(|request| request.model == ModelRef::new("local", "test-model"))
        );
        assert!(requests[1].messages.iter().any(|message| {
            message.content.iter().any(|block|
            matches!(block, ProviderBlock::ToolResult { call_id, .. } if call_id == "call-1")
        )
        }));
    }

    #[tokio::test]
    async fn provider_open_retry_and_stream_protocol_are_typed() {
        let unavailable = ProviderError::new(
            ProviderErrorKind::Unavailable,
            ProviderErrorPhase::Open,
            "temporarily unavailable",
        );
        let provider = Arc::new(ScriptedProvider::new([
            Err(unavailable),
            Ok(completed_events(
                vec![ProviderBlock::Text { text: "ok".into() }],
                ProviderStopReason::EndTurn,
            )),
        ]));
        let loop_ = AgentLoop::builder()
            .provider(provider.clone())
            .provider_model(provider_model())
            .max_retries(1)
            .build()
            .unwrap();
        assert!(run(&loop_, vec![MessageParam::user("retry")]).await.is_ok());
        {
            let requests = provider.requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].request_id, requests[1].request_id);
        }

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let empty: ModelEventStream = Box::pin(futures_util::stream::empty());
        let error = consume_provider_stream(empty, ModelRef::new("local", "test-model"), &tx)
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentLoopError::Provider { source, .. } if source.kind == ProviderErrorKind::Protocol)
        );

        let events = completed_events(Vec::new(), ProviderStopReason::EndTurn)
            .into_iter()
            .chain([Ok(ModelStreamEvent::TextDelta("late".into()))]);
        let stream: ModelEventStream = Box::pin(futures_util::stream::iter(events));
        let error = consume_provider_stream(stream, ModelRef::new("local", "test-model"), &tx)
            .await
            .unwrap_err();
        assert!(
            matches!(error, AgentLoopError::Provider { source, .. } if source.kind == ProviderErrorKind::Protocol)
        );
    }

    #[test]
    fn provider_builder_rejects_missing_and_mixed_backends() {
        let provider = || Arc::new(FakeProvider { _secret: "secret" }) as Arc<dyn ModelProvider>;
        assert!(matches!(
            AgentLoop::builder().provider(provider()).build(),
            Err(AgentLoopError::Builder(message)) if message.contains("provider model")
        ));
        assert!(matches!(
            AgentLoop::builder().provider_model(provider_model()).build(),
            Err(AgentLoopError::Builder(message)) if message.contains("provider is required")
        ));
        assert!(matches!(
            AgentLoop::builder()
                .client(test_client())
                .model(test_model())
                .provider(provider())
                .provider_model(provider_model())
                .build(),
            Err(AgentLoopError::Builder(message)) if message.contains("cannot be mixed")
        ));
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
    fn reasoning_effort_builds_a_capability_checked_budget() {
        let model = ModelInfo::builder()
            .id("thinking-model")
            .context_window(200_000)
            .max_output_tokens(8_192)
            .capability(ModelCapabilities::EXTENDED_THINKING)
            .build()
            .expect("model");
        let loop_ = AgentLoop::builder()
            .client(test_client())
            .model(model)
            .reasoning_effort(sylvander_protocol::ReasoningEffort::High)
            .build()
            .expect("loop");
        let request = loop_.build_request(&[MessageParam::user("think")]);
        assert_eq!(request.thinking.unwrap().budget_tokens, 8_192);
        assert_eq!(
            loop_.reasoning_effort(),
            sylvander_protocol::ReasoningEffort::High
        );
    }

    #[test]
    fn retry_cause_distinguishes_rate_limit_server_and_stream_failures() {
        let api = |status| AnthropicError::Api {
            status,
            error_type: "test".into(),
            error_message: "failed".into(),
            request_id: None,
        };
        assert_eq!(
            retry_cause(&api(429)),
            sylvander_protocol::RetryCause::RateLimit
        );
        assert_eq!(
            retry_cause(&api(503)),
            sylvander_protocol::RetryCause::Server
        );
        assert_eq!(
            retry_cause(&AnthropicError::SseParse {
                message: "truncated".into(),
                position: 10,
            }),
            sylvander_protocol::RetryCause::Stream
        );
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
    fn cumulative_usage_saturates_and_preserves_optional_cache_semantics() {
        let total = Usage {
            input_tokens: u32::MAX - 1,
            output_tokens: 10,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(u32::MAX),
        };
        let next = Usage {
            input_tokens: 10,
            output_tokens: u32::MAX,
            cache_creation_input_tokens: Some(4),
            cache_read_input_tokens: None,
        };

        let cumulative = saturating_add_usage(&total, &next);
        assert_eq!(cumulative.input_tokens, u32::MAX);
        assert_eq!(cumulative.output_tokens, u32::MAX);
        assert_eq!(cumulative.cache_creation_input_tokens, Some(4));
        assert_eq!(cumulative.cache_read_input_tokens, Some(u32::MAX));
        assert_eq!(saturating_add_optional_tokens(None, None), None);
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
