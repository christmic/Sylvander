//! `AgentLoop` — the OOP class-based async driver for the agent loop.
//!
//! Populated across multiple commits:
//! - A5 (this commit): data types + builder pattern (no `run()` yet)
//! - A6: `run()` + `run_stream()` + reactive event emission
//! - A7: iteration limit + retry/backoff + capability validation
//!
//! See `projects/Sylvander/designs/sylvander-agent-design.md` for the
//! full design.

use std::sync::Arc;

use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::ModelInfo;
use sylvander_llm_anthropic::api::types::Message;

use super::compress::Compressor;
use super::error::AgentLoopError;
use super::event::AgentEvent;
use super::tool::ToolRegistry;

/// The agent loop. Holds the LLM client, resolved model, tools, and
/// configuration. Drives the `while` iteration in `run()`.
pub struct AgentLoop {
    #[allow(dead_code)] // used in A6 (run)
    pub(crate) client: AnthropicClient,
    pub(crate) model: ModelInfo,
    pub(crate) tools: ToolRegistry,
    pub(crate) compressor: Arc<dyn Compressor>,
    pub(crate) max_iterations: u32,
    pub(crate) max_retries: u32,
    pub(crate) on_event: Option<Box<dyn FnMut(AgentEvent) + Send>>,
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

    // `run()` and `run_stream()` land in A6.
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