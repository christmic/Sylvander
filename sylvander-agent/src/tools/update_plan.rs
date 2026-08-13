//! `update_plan` marker tool intercepted by the Agent loop.

use async_trait::async_trait;
use sylvander_llm_core::InputSchema;

use crate::execution::tool_context::ToolContext;
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};

#[derive(Default)]
pub struct UpdatePlanTool;

impl UpdatePlanTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ToolDefinition for UpdatePlanTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::strict(
            "update_plan",
            "Update the visible approved plan and its zero-based current step as work progresses.",
            InputSchema::new_with_properties(
                serde_json::json!({
                    "plan_id": {"type": "string"},
                    "steps": {"type": "array", "minItems": 1, "items": {"type": "string"}},
                    "current": {"type": "integer", "minimum": 0}
                }),
                &["plan_id", "steps", "current"],
            )
            .schema,
            crate::tool_invocation::ToolInvocationClass::Control,
        )
    }
}

#[async_trait]
impl ToolExecutor for UpdatePlanTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Other(
            "update_plan must be intercepted at the loop level".into(),
        ))
    }
}
