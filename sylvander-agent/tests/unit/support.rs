use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value as JsonValue;
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

use crate::compress::disk::{DiskHandle, ToolResultDisk};
use crate::run::{AgentRun, AgentRunBuilder};
use crate::spec::AgentSpec;
use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

pub(crate) fn provider_capabilities(
    capabilities: AnthropicModelCapabilities,
) -> ProviderModelCapabilities {
    let mut provider_capabilities = ProviderModelCapabilities::empty();
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
        if capabilities.contains(anthropic) {
            provider_capabilities = provider_capabilities | provider;
        }
    }
    provider_capabilities
}

pub(crate) fn exact_anthropic_model(
    provider_id: &str,
    model: &AnthropicModelInfo,
) -> ProviderModelInfo {
    ProviderModelInfo {
        reference: ModelRef::new(provider_id, &model.id),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities: provider_capabilities(model.capabilities),
    }
}

pub(crate) fn qualified_anthropic_run_builder(
    spec: AgentSpec,
    client: AnthropicClient,
) -> AgentRunBuilder {
    qualified_anthropic_run_builder_with_capabilities(
        spec,
        client,
        AnthropicModelCapabilities::empty(),
    )
}

pub(crate) fn qualified_anthropic_run_builder_with_capabilities(
    spec: AgentSpec,
    client: AnthropicClient,
    capabilities: AnthropicModelCapabilities,
) -> AgentRunBuilder {
    let provider_id = spec.model.provider.clone();
    let model = spec.to_model_info().expect("valid test Agent model");
    let mut exact = exact_anthropic_model(&provider_id, &model);
    exact.capabilities = provider_capabilities(capabilities);
    AgentRun::qualified_router_builder(
        spec,
        Arc::new(AnthropicProvider::new(&provider_id, client)),
        exact,
    )
}

/// In-memory tool double shared by white-box unit tests.
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

    pub(crate) fn with_responses(mut self, responses: Vec<ToolOutput>) -> Self {
        self.responses = responses;
        self
    }

    pub(crate) fn calls(&self) -> Vec<JsonValue> {
        self.calls.lock().expect("MockTool lock poisoned").clone()
    }

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

/// In-memory oversized-result sink shared by white-box unit tests.
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
