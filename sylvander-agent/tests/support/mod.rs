#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sylvander_agent::compress::disk::{DiskHandle, ToolResultDisk};
use sylvander_agent::prelude::{AgentLoop, AgentLoopBuilder};
use sylvander_agent::tool::{Tool, ToolError, ToolOutput};
use sylvander_agent::tool_context::ToolContext;
use sylvander_llm_anthropic::{
    AnthropicProvider,
    api::{
        client::AnthropicClient,
        model::{ModelCapabilities as AnthropicModelCapabilities, ModelInfo as AnthropicModelInfo},
        types::InputSchema,
    },
};
use sylvander_llm_core::{
    ModelCapabilities as ProviderModelCapabilities, ModelInfo as ProviderModelInfo, ModelRef,
};

/// Build an Agent loop through the sole current provider-qualified API.
pub(crate) fn qualified_anthropic_loop_builder(
    client: AnthropicClient,
    model: AnthropicModelInfo,
) -> AgentLoopBuilder {
    assert!(
        model.cache_ttl.is_empty(),
        "provider-neutral test models cannot carry Anthropic-only cache TTL metadata"
    );

    let mut capabilities = ProviderModelCapabilities::empty();
    for (anthropic, provider) in [
        (
            AnthropicModelCapabilities::EXTENDED_THINKING,
            ProviderModelCapabilities::REASONING,
        ),
        (
            AnthropicModelCapabilities::PROMPT_CACHING,
            ProviderModelCapabilities::PROMPT_CACHING,
        ),
        (
            AnthropicModelCapabilities::STRUCTURED_OUTPUT,
            ProviderModelCapabilities::STRUCTURED_OUTPUT,
        ),
        (
            AnthropicModelCapabilities::TOOL_USE,
            ProviderModelCapabilities::TOOL_USE,
        ),
        (
            AnthropicModelCapabilities::VISION,
            ProviderModelCapabilities::VISION,
        ),
        (
            AnthropicModelCapabilities::DOCUMENT_INPUT,
            ProviderModelCapabilities::DOCUMENT_INPUT,
        ),
    ] {
        if model.capabilities.contains(anthropic) {
            capabilities = capabilities | provider;
        }
    }

    let provider_model = ProviderModelInfo {
        reference: ModelRef::new("anthropic", model.id),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities,
    };

    AgentLoop::builder()
        .qualified_router(Arc::new(AnthropicProvider::new("anthropic", client)))
        .provider_model(provider_model)
}

/// In-memory tool double for public-contract integration tests.
#[derive(Debug, Clone)]
pub(crate) struct MockTool {
    name: String,
    description: String,
    schema: InputSchema,
    responses: Vec<ToolOutput>,
    calls: Arc<Mutex<Vec<JsonValue>>>,
}

impl MockTool {
    pub(crate) fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        response: ToolOutput,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema: InputSchema::empty(),
            responses: vec![response],
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn with_schema(mut self, schema: InputSchema) -> Self {
        self.schema = schema;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn with_responses(mut self, responses: Vec<ToolOutput>) -> Self {
        self.responses = responses;
        self
    }

    #[allow(dead_code)]
    pub(crate) fn calls(&self) -> Vec<JsonValue> {
        self.calls.lock().expect("MockTool lock poisoned").clone()
    }

    #[allow(dead_code)]
    pub(crate) fn call_count(&self) -> usize {
        self.calls.lock().expect("MockTool lock poisoned").len()
    }
}

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> InputSchema {
        self.schema.clone()
    }

    async fn execute(&self, _ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        let index = {
            let mut calls = self.calls.lock().expect("MockTool lock poisoned");
            calls.push(input);
            calls.len() - 1
        };
        self.responses
            .get(index)
            .or_else(|| self.responses.last())
            .cloned()
            .ok_or_else(|| ToolError::Other("no responses configured".into()))
    }
}

/// In-memory oversized-result sink for public-contract integration tests.
#[derive(Default, Clone)]
pub(crate) struct InMemoryToolResultDisk {
    inner: Arc<Mutex<HashMap<String, String>>>,
    write_count: Arc<Mutex<usize>>,
}

impl InMemoryToolResultDisk {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn get(&self, tool_use_id: &str) -> Option<String> {
        self.inner.lock().unwrap().get(tool_use_id).cloned()
    }

    pub(crate) fn write_count(&self) -> usize {
        *self.write_count.lock().unwrap()
    }

    #[allow(dead_code)]
    pub(crate) fn ids(&self) -> Vec<String> {
        let mut ids = self
            .inner
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        ids.sort();
        ids
    }
}

impl ToolResultDisk for InMemoryToolResultDisk {
    fn persist(&self, tool_use_id: &str, body: &str) -> io::Result<DiskHandle> {
        self.inner
            .lock()
            .unwrap()
            .insert(tool_use_id.to_owned(), body.to_owned());
        *self.write_count.lock().unwrap() += 1;
        Ok(DiskHandle {
            path: PathBuf::from(format!("<in-memory>/{tool_use_id}")),
            original_bytes: body.len(),
        })
    }
}
