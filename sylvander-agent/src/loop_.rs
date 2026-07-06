//! `AgentLoop` — the OOP class-based async driver for the agent loop.
//!
//! # Architecture
//!
//! The loop logic lives in three module-level free functions:
//! - [`run`] — consumes the stream, returns `Result<AgentRun, _>`
//! - [`run_stream`] — the single source of truth: drives the
//!   iteration, yields `AgentEvent`s
//! - [`run_with_events`] — consumes the stream, fires events into a
//!   callback, returns the final `AgentRun`
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

use super::compress::Compressor;
use super::error::AgentLoopError;
use super::event::AgentEvent;
use super::tool::ToolRegistry;

/// The agent loop. Holds the LLM client, resolved model, tools, and
/// configuration. Iteration logic is in the free functions [`run`],
/// [`run_stream`], and [`run_with_events`].
pub struct AgentLoop {
    pub(crate) client: AnthropicClient,
    pub(crate) model: ModelInfo,
    pub(crate) tools: ToolRegistry,
    pub(crate) compressor: Arc<dyn Compressor>,
    pub(crate) max_iterations: u32,
    pub(crate) max_retries: u32,
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
pub struct AgentRun {
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
    compressor: Option<Arc<dyn Compressor>>,
    max_iterations: u32,
    max_retries: u32,
}

impl Default for AgentLoopBuilder {
    fn default() -> Self {
        Self {
            client: None,
            model: None,
            tools: ToolRegistry::new(),
            compressor: None,
            max_iterations: 50,
            max_retries: 3,
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

    /// Set the compression strategy (defaults to `NoCompression`).
    #[must_use]
    pub fn compressor<C: Compressor + 'static>(mut self, compressor: C) -> Self {
        self.compressor = Some(Arc::new(compressor));
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
        let compressor = self
            .compressor
            .unwrap_or_else(|| Arc::new(super::compress::NoCompression));

        Ok(AgentLoop {
            client,
            model,
            tools: self.tools,
            compressor,
            max_iterations: self.max_iterations,
            max_retries: self.max_retries,
        })
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

    /// Borrow the configured compression strategy.
    #[must_use]
    pub fn compressor(&self) -> &dyn Compressor {
        self.compressor.as_ref()
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

            // 1. Compression (best-effort, legacy single-strategy path)
            {
                let mut compress_ctx = super::compress::CompressContext {
                    messages: &mut messages,
                    last_usage: &total_usage,
                    model_info: &config.model,
                };
                let outcome = config.compressor.maybe_compress(&mut compress_ctx);
                if let Some(layer) =
                    super::compress::outcome_to_layer_report(config.compressor.name(), &outcome)
                {
                    yield AgentEvent::Compressed {
                        layers: vec![layer],
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

            // 4. Call LLM with retry on transient errors
            let response = match config.call_llm_with_retry(&request).await {
                Ok(r) => r,
                Err(e) => {
                    yield AgentEvent::Error(e);
                    break;
                }
            };

            // 5. Emit text / thinking chunks
            for block in &response.content {
                match block {
                    ContentBlock::Text(t) => {
                        yield AgentEvent::TextChunk(t.text.clone());
                    }
                    ContentBlock::Thinking(t) => {
                        yield AgentEvent::ThinkingChunk(t.thinking.clone());
                    }
                    ContentBlock::ToolUse(_) => {}
                }
            }

            // Capture state we need after re-feeding
            let response_stop_reason = response.stop_reason;
            let response_id = response.id.clone();
            total_usage = response.usage.clone();

            // 6. Re-feed assistant message
            messages.push(assistant_message_from_response(&response));

            yield AgentEvent::IterationEnd {
                iteration,
                usage: total_usage.clone(),
            };

            // 7. Check stop_reason
            let should_continue = match response_stop_reason {
                Some(
                    StopReason::EndTurn
                    | StopReason::StopSequence
                    | StopReason::MaxTokens
                    | StopReason::Refusal
                    | StopReason::PauseTurn,
                ) => {
                    final_message = Some(Message {
                        id: response_id,
                        kind: sylvander_llm_anthropic::api::types::MessageKind::Message,
                        role: MessageRole::Assistant,
                        content: response.content.clone(),
                        model: config.model.id.clone(),
                        stop_reason: response_stop_reason,
                        stop_sequence: None,
                        usage: total_usage.clone(),
                    });
                    false
                }
                Some(StopReason::ToolUse) | None => true,
            };

            if !should_continue {
                break;
            }

            // 8. Execute tools if any tool_use blocks
            let tool_blocks: Vec<&ToolUseBlock> = response
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolUse(t) => Some(t),
                    _ => None,
                })
                .collect();

            if tool_blocks.is_empty() {
                // stop_reason said ToolUse but no actual tool_use block.
                // Treat as end.
                final_message = Some(response);
                break;
            }

            let mut tool_result_blocks = Vec::with_capacity(tool_blocks.len());
            for tool_use in tool_blocks {
                yield AgentEvent::ToolCallStart {
                    id: tool_use.id.clone(),
                    name: tool_use.name.clone(),
                    input: tool_use.input.clone(),
                };

                let (output, is_error) = if let Some(tool) =
                    config.tools.get(tool_use.name.as_str())
                {
                    match tool.execute(tool_use.input.clone()).await {
                        Ok(out) => (out.content, out.is_error),
                        Err(e) => {
                            warn!(tool = %tool_use.name, error = %e, "tool execution failed");
                            (format!("tool execution failed: {e}"), true)
                        }
                    }
                } else {
                    warn!(tool = %tool_use.name, "tool not found in registry");
                    (
                        format!("tool `{}` not found in registry", tool_use.name),
                        true,
                    )
                };

                yield AgentEvent::ToolCallEnd {
                    id: tool_use.id.clone(),
                    name: tool_use.name.clone(),
                    output: output.clone(),
                    is_error,
                };

                tool_result_blocks.push(UserContentBlock::ToolResult(
                    ToolResultBlock::new(tool_use.id.clone(), output).with_error(is_error),
                ));
            }

            messages.push(MessageParam::user_blocks(tool_result_blocks));
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
/// event stream and returns the final [`AgentRun`].
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
) -> Result<AgentRun, AgentLoopError> {
    let max_iterations = config.max_iterations;
    consume_stream_to_run(max_iterations, run_stream(config, initial_messages)).await
}

/// Convenience wrapper around [`run_stream`] that fires every event
/// into the supplied callback, then returns the final [`AgentRun`].
/// Terminal `Done` / `Error` events are extracted into the return
/// value rather than fired to the callback.
pub async fn run_with_events<F>(
    config: &AgentLoop,
    initial_messages: Vec<MessageParam>,
    mut on_event: F,
) -> Result<AgentRun, AgentLoopError>
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

    Ok(AgentRun {
        final_message,
        iterations,
        total_usage,
    })
}

// =====================================================================
// Internal helpers on AgentLoop (private methods used by run_stream)
// =====================================================================

impl AgentLoop {
    /// Call the LLM with retry/backoff on transient errors.
    async fn call_llm_with_retry(
        &self,
        request: &CreateMessageRequest,
    ) -> Result<Message, AgentLoopError> {
        let mut last_err: Option<AnthropicError> = None;
        let max_attempts = self.max_retries + 1;
        for attempt in 0..max_attempts {
            match self.client.messages().create(request).await {
                Ok(msg) => return Ok(msg),
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

        if !self.tools.is_empty() {
            builder = builder.tools(self.tools.definitions());
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
/// accumulate final state, return `AgentRun` or `Err`.
async fn consume_stream_to_run(
    max_iterations: u32,
    stream: impl Stream<Item = AgentEvent> + Send,
) -> Result<AgentRun, AgentLoopError> {
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
    Ok(AgentRun {
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
    fn default_compressor_is_no_compression() {
        let loop_ = AgentLoop::builder()
            .client(test_client())
            .model(test_model())
            .build()
            .expect("build");
        use super::super::compress::{CompressContext, CompressionOutcome};
        use sylvander_llm_anthropic::api::types::Usage;
        let mut messages = vec![];
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut ctx = CompressContext {
            messages: &mut messages,
            last_usage: &usage,
            model_info: loop_.model(),
        };
        let outcome = loop_.compressor().maybe_compress(&mut ctx);
        assert_eq!(outcome, CompressionOutcome::Keep);
    }

    #[test]
    fn agent_run_debug_impl() {
        let run = AgentRun {
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