//! `present_plan` marker tool intercepted by the Agent loop.

use async_trait::async_trait;
use sylvander_llm_core::InputSchema;

use crate::execution::tool_context::ToolContext;
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};

#[derive(Default)]
pub struct PresentPlanTool;

impl PresentPlanTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ToolDefinition for PresentPlanTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::strict(
            "present_plan",
            "Present an ordered implementation plan for explicit user review before proceeding.",
            InputSchema::new_with_properties(
                serde_json::json!({
                    "steps": {
                        "type": "array",
                        "minItems": 1,
                        "items": { "type": "string" },
                        "description": "Ordered, concrete implementation steps."
                    }
                }),
                &["steps"],
            )
            .schema,
            crate::tool::invocation::ToolInvocationClass::Control,
        )
    }
}

#[async_trait]
impl ToolExecutor for PresentPlanTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Other(
            "present_plan must be intercepted at the loop level".into(),
        ))
    }
}
