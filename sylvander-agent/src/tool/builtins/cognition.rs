//! `consult_cognition` marker tool intercepted by the Agent loop.

use async_trait::async_trait;
use sylvander_llm_core::InputSchema;

use crate::execution::tool_context::ToolContext;
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};

#[derive(Default)]
pub struct ConsultCognitionTool;

impl ConsultCognitionTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ToolDefinition for ConsultCognitionTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::strict(
            "consult_cognition",
            "Request one bounded advisory pass from an approved internal cognitive role. Use it only when another draft, deeper analysis, or critique materially helps; you remain responsible for the final answer.",
            InputSchema::from_json_value(serde_json::json!({
                "type": "object",
                "properties": {
                    "role": {
                        "type": "string",
                        "enum": ["fast_draft", "deliberation", "critic"]
                    },
                    "prompt": {"type": "string", "minLength": 1, "maxLength": 32768}
                },
                "required": ["role", "prompt"],
                "additionalProperties": false
            }))
            .schema,
            crate::tool::invocation::ToolInvocationClass::Control,
        )
    }
}

#[async_trait]
impl ToolExecutor for ConsultCognitionTool {
    async fn handle(
        &self,
        _context: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Other(
            "consult_cognition must be intercepted at the loop level".into(),
        ))
    }
}
