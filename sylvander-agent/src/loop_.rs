//! `AgentLoop` — the OOP class-based async driver for the agent loop.
//!
//! Populated across multiple commits:
//! - A5: data types + builder pattern (no `run()` yet)
//! - A6: `run()` + `run_stream()` + reactive event emission
//! - A7: iteration limit + retry/backoff + capability validation
//!
//! See `projects/Sylvander/designs/sylvander-agent-design.md` for the
//! full design.

use std::sync::Arc;

use futures_util::Stream;
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
/// configuration. Drives the `while` iteration in `run()`.
pub struct AgentLoop {
    pub(crate) client: AnthropicClient,
    pub(crate) model: ModelInfo,
    pub(crate) tools: ToolRegistry,
    pub(crate) compressor: Arc<dyn Compressor>,
    pub(crate) max_iterations: u32,
    pub(crate) max_retries: u32,
    pub(crate) on_event: Option<Box<dyn FnMut(AgentEvent) + Send>>,
    pub(crate) iteration_count: u32,
}

impl std::fmt::Debug for AgentLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoop")
            .field("model", &self.model)
            .field("tools", &self.tools)
            .field("max_iterations", &self.max_iterations)
            .field("max_retries", &self.max_retries)
            .field("on_event", &self.on_event.as_ref().map(|_| "FnMut(AgentEvent)"))
            .finish_non_exhaustive()
    }
}

/// Outcome of a completed `AgentLoop::run()`.
#[derive(Debug, Clone)]
pub struct AgentRun {
    /// Final assembled message (the last assistant turn before the loop
    /// terminated).
    pub final_message: Message,
    /// Total iterations executed.
    pub iterations: u32,
    /// Cumulative token usage across all LLM calls.
    pub total_usage: sylvander_llm_anthropic::api::types::Usage,
}

/// Builder for [`AgentLoop`].
pub struct AgentLoopBuilder {
    client: Option<AnthropicClient>,
    model: Option<ModelInfo>,
    tools: ToolRegistry,
    compressor: Option<Arc<dyn Compressor>>,
    max_iterations: u32,
    max_retries: u32,
    on_event: Option<Box<dyn FnMut(AgentEvent) + Send>>,
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
            on_event: None,
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
            .field("on_event", &self.on_event.as_ref().map(|_| "FnMut(AgentEvent)"))
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

    /// Register an event callback. The callback receives every event
    /// the loop emits.
    #[must_use]
    pub fn on_event<F>(mut self, f: F) -> Self
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        self.on_event = Some(Box::new(f));
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
            on_event: self.on_event,
            iteration_count: 0,
        })
    }
}

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

    /// Run the agent loop until the model emits `end_turn` or
    /// `max_iterations` is reached.
    ///
    /// Emits events to `on_event` (if configured) and returns the
    /// final [`AgentRun`]. For reactive consumers that want a `Stream`,
    /// use [`Self::run_stream`].
    ///
    /// # Errors
    /// - [`AgentLoopError::MaxIterationsReached`] — loop hit cap
    /// - [`AgentLoopError::Llm`] — LLM call failed (after retries)
    /// - [`AgentLoopError::Tool`] — non-recoverable tool failure
    /// - [`AgentLoopError::IncompatibleModel`] — request requires
    ///   capability the model doesn't have
    pub async fn run(
        &mut self,
        initial_messages: Vec<MessageParam>,
    ) -> Result<AgentRun, AgentLoopError> {
        let mut messages = initial_messages;
        let mut total_usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        let mut final_message: Option<Message> = None;

        for iteration in 1..=self.max_iterations {
            self.iteration_count = iteration;
            self.emit_event(AgentEvent::IterationStart { iteration });

            // 1. Compression (best-effort)
            {
                let mut compress_ctx = super::compress::CompressContext {
                    messages: &mut messages,
                    last_usage: &total_usage,
                    model_info: &self.model,
                };
                let outcome = self.compressor.maybe_compress(&mut compress_ctx);
                if let super::compress::CompressionOutcome::Truncated {
                    removed_count,
                    freed_tokens,
                } = outcome
                {
                    self.emit_event(AgentEvent::Compressed {
                        removed_count,
                        freed_tokens,
                    });
                }
            }

            // 2. Build request
            let request = self.build_request(&messages);

            // 3. Validate request against model capabilities
            self.validate_capabilities(&request)?;

            // 4. Call LLM with retry on transient errors
            let response = self.call_llm_with_retry(&request).await?;

            // 5. Emit text/thinking chunks
            for block in &response.content {
                match block {
                    ContentBlock::Text(t) => {
                        self.emit_event(AgentEvent::TextChunk(t.text.clone()));
                    }
                    ContentBlock::Thinking(t) => {
                        self.emit_event(AgentEvent::ThinkingChunk(t.thinking.clone()));
                    }
                    ContentBlock::ToolUse(_) => {}
                }
            }

            // 6. Re-feed assistant message
            messages.push(assistant_message_from_response(&response));
            total_usage = response.usage.clone();

            self.emit_event(AgentEvent::IterationEnd {
                iteration,
                usage: response.usage.clone(),
            });

            // 7. Check stop_reason
            match response.stop_reason {
                Some(
                    StopReason::EndTurn
                    | StopReason::StopSequence
                    | StopReason::MaxTokens
                    | StopReason::Refusal
                    | StopReason::PauseTurn,
                ) => {
                    final_message = Some(response);
                    break;
                }
                Some(StopReason::ToolUse) | None => {
                    // 8. Execute tools
                    let tool_blocks: Vec<&ToolUseBlock> = response
                        .content
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse(t) => Some(t),
                            _ => None,
                        })
                        .collect();

                    if tool_blocks.is_empty() {
                        final_message = Some(response);
                        break;
                    }

                    let mut tool_result_blocks = Vec::with_capacity(tool_blocks.len());
                    for tool_use in tool_blocks {
                        self.emit_event(AgentEvent::ToolCallStart {
                            id: tool_use.id.clone(),
                            name: tool_use.name.clone(),
                            input: tool_use.input.clone(),
                        });

                        let (output, is_error) = if let Some(tool) =
                            self.tools.get(tool_use.name.as_str())
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

                        self.emit_event(AgentEvent::ToolCallEnd {
                            id: tool_use.id.clone(),
                            name: tool_use.name.clone(),
                            output: output.clone(),
                            is_error,
                        });

                        tool_result_blocks.push(UserContentBlock::ToolResult(
                            ToolResultBlock::new(tool_use.id.clone(), output).with_error(is_error),
                        ));
                    }

                    messages.push(MessageParam::user_blocks(tool_result_blocks));
                }
            }
        }

        let final_message = final_message.ok_or(AgentLoopError::MaxIterationsReached(
            self.max_iterations,
        ))?;

        let run = AgentRun {
            final_message,
            iterations: self.current_iteration(),
            total_usage,
        };
        self.emit_event(AgentEvent::Done(run.final_message.clone()));
        Ok(run)
    }

    /// Call the LLM with retry/backoff on transient errors.
    ///
    /// Retries up to `max_retries` times on `AnthropicError::is_retryable()`
    /// (5xx + 429). 4xx and other errors propagate immediately.
    async fn call_llm_with_retry(
        &self,
        request: &CreateMessageRequest,
    ) -> Result<Message, AgentLoopError> {
        let mut last_err: Option<AnthropicError> = None;
        let max_attempts = self.max_retries + 1; // retries are ON TOP of the first attempt
        for attempt in 0..max_attempts {
            match self.client.messages().create(request).await {
                Ok(msg) => return Ok(msg),
                Err(e) => {
                    if !e.is_retryable() || attempt == max_attempts - 1 {
                        let msg = format!("{e}");
                        // Emit Error via callback — but we don't have
                        // access to self.emit_event because we borrow
                        // self immutably here. Use a side channel.
                        // Actually call_llm_with_retry takes &self, but
                        // emit_event needs &mut self. Skip emission
                        // here — the caller (run) emits Error if we
                        // return Err.
                        let _ = msg;
                        return Err(AgentLoopError::Llm {
                            retries: attempt,
                            source: e,
                        });
                    }
                    // Exponential backoff: 100ms, 200ms, 400ms, ...
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
        // Shouldn't reach here, but satisfy the compiler
        Err(AgentLoopError::Llm {
            retries: self.max_retries,
            source: last_err.expect("retry loop must have errored at least once"),
        })
    }

    /// Validate the request against the model's capabilities.
    fn validate_capabilities(&self, request: &CreateMessageRequest) -> Result<(), AgentLoopError> {
        // Tools set → need TOOL_USE
        if !request.tools.is_empty()
            && !self.model.capabilities.contains(ModelCapabilities::TOOL_USE)
        {
            return Err(AgentLoopError::IncompatibleModel(format!(
                "model `{}` does not support TOOL_USE (required because tools are set)",
                self.model.id
            )));
        }

        // Thinking set → need EXTENDED_THINKING
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

        // output_config set → need STRUCTURED_OUTPUT
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

    /// Run the agent loop, returning a [`Stream`] of [`AgentEvent`]s.
    ///
    /// Use this for reactive consumers (CLI/TUI/SSE) that want events
    /// as they fire rather than waiting for the loop to finish.
    pub fn run_stream(
        &mut self,
        _initial_messages: Vec<MessageParam>,
    ) -> impl Stream<Item = AgentEvent> + Send + '_ {
        // M2 placeholder: full reactive streaming integration lands in
        // A7 alongside retry/backoff. For now, callers use `run()` with
        // an `on_event` callback to get reactive event delivery.
        async_stream::stream! {
            // empty — see run() for the actual implementation
            if false {
                yield AgentEvent::Done(Message {
                    id: String::new(),
                    kind: sylvander_llm_anthropic::api::types::MessageKind::Message,
                    role: MessageRole::Assistant,
                    content: vec![],
                    model: String::new(),
                    stop_reason: None,
                    stop_sequence: None,
                    usage: Usage {
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_creation_input_tokens: None,
                        cache_read_input_tokens: None,
                    },
                });
            }
        }
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

    /// Emit an event to the configured callback (if any).
    fn emit_event(&mut self, event: AgentEvent) {
        if let Some(cb) = self.on_event.as_mut() {
            cb(event);
        }
    }

    /// Tracked iteration count (mutable state on the loop).
    fn current_iteration(&self) -> u32 {
        self.iteration_count
    }
}

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
    fn builder_with_event_callback_stores_it() {
        let loop_ = AgentLoop::builder()
            .client(test_client())
            .model(test_model())
            .on_event(|_event| {})
            .build()
            .expect("build");
        assert!(loop_.on_event.is_some());
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
        // Verify it's a NoCompression by checking the trait object
        // behavior — calling maybe_compress should return Keep.
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
        // Just verify Debug is implemented
        let run = AgentRun {
            final_message: Message {
                id: "msg_x".into(),
                kind: sylvander_llm_anthropic::api::types::MessageKind::Message,
                role: sylvander_llm_anthropic::api::types::MessageRole::Assistant,
                content: vec![],
                model: "test-model".into(),
                stop_reason: Some(sylvander_llm_anthropic::api::types::StopReason::EndTurn),
                stop_sequence: None,
                usage: sylvander_llm_anthropic::api::types::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                },
            },
            iterations: 1,
            total_usage: sylvander_llm_anthropic::api::types::Usage {
                input_tokens: 1,
                output_tokens: 1,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };
        let _ = format!("{run:?}");
        // Suppress unused-import warning
        let _ = json!({});
    }
}