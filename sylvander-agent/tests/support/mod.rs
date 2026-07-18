use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sylvander_agent::tool::{Tool, ToolError, ToolOutput};
use sylvander_agent::tool_context::ToolContext;
use sylvander_llm_anthropic::api::types::InputSchema;

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
