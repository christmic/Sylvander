use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sylvander_llm_core::InputSchema;

use crate::artifact::{ArtifactReference, ArtifactStoreError, ArtifactWrite, TurnArtifactStore};
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};
use crate::tool_context::ToolContext;

/// In-memory tool double shared by white-box unit tests.
#[derive(Debug, Clone)]
pub(crate) struct MockTool {
    name: String,
    description: String,
    schema: InputSchema,
    prompt_guidelines: Vec<String>,
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
            prompt_guidelines: Vec::new(),
            responses: vec![response],
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn with_responses(mut self, responses: Vec<ToolOutput>) -> Self {
        self.responses = responses;
        self
    }

    pub(crate) fn with_prompt_guidelines(
        mut self,
        guidelines: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.prompt_guidelines = guidelines.into_iter().map(Into::into).collect();
        self
    }

    pub(crate) fn calls(&self) -> Vec<JsonValue> {
        self.calls.lock().expect("MockTool lock poisoned").clone()
    }

    pub(crate) fn call_count(&self) -> usize {
        self.calls.lock().expect("MockTool lock poisoned").len()
    }
}

impl ToolDefinition for MockTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::immediate(
            self.name.clone(),
            self.description.clone(),
            self.schema.schema.clone(),
            crate::tool_invocation::ToolInvocationClass::Extension,
        )
        .with_prompt_guidelines(self.prompt_guidelines.clone())
    }
}

#[async_trait]
impl ToolExecutor for MockTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        let index = {
            let mut calls = self.calls.lock().expect("MockTool lock poisoned");
            calls.push(call.input().clone());
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
pub(crate) struct InMemoryArtifactStore {
    inner: Arc<Mutex<HashMap<String, String>>>,
    write_count: Arc<Mutex<usize>>,
}

impl InMemoryArtifactStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn get(&self, tool_use_id: &str) -> Option<String> {
        self.inner.lock().unwrap().get(tool_use_id).cloned()
    }
}

#[async_trait]
impl TurnArtifactStore for InMemoryArtifactStore {
    async fn persist(
        &self,
        artifact: ArtifactWrite,
    ) -> Result<ArtifactReference, ArtifactStoreError> {
        let original_bytes = artifact.payload.len();
        let body =
            String::from_utf8(artifact.payload).map_err(|_| ArtifactStoreError::InvalidRequest)?;
        self.inner
            .lock()
            .unwrap()
            .insert(artifact.call_id.clone(), body);
        *self.write_count.lock().unwrap() += 1;
        Ok(ArtifactReference {
            locator: format!("artifact:{}", artifact.call_id),
            original_bytes,
        })
    }
}
