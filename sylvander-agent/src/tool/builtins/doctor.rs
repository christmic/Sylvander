//! `inspect_runtime` marker tool intercepted by the Agent loop.

use async_trait::async_trait;
use sylvander_llm_core::InputSchema;

use crate::execution::tool_context::ToolContext;
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};

#[derive(Default)]
pub struct InspectRuntimeTool;

impl InspectRuntimeTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ToolDefinition for InspectRuntimeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::strict(
            "inspect_runtime",
            "Read a content-safe snapshot of this Session's Agent, task, workspace, recovery, and governance environment. This tool cannot mutate Runtime state.",
            InputSchema::from_json_value(serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }))
            .schema,
            crate::tool::invocation::ToolInvocationClass::Read,
        )
    }
}

#[async_trait]
impl ToolExecutor for InspectRuntimeTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Other(
            "inspect_runtime must be intercepted at the loop level".into(),
        ))
    }
}
