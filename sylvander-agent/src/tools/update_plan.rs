//! `update_plan` marker tool intercepted by the Agent loop.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

#[derive(Default)]
pub struct UpdatePlanTool;

impl UpdatePlanTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for UpdatePlanTool {
    fn name(&self) -> &'static str {
        "update_plan"
    }

    fn description(&self) -> &'static str {
        "Update the visible approved plan and its zero-based current step as work progresses."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            serde_json::json!({
                "plan_id": {"type": "string"},
                "steps": {"type": "array", "minItems": 1, "items": {"type": "string"}},
                "current": {"type": "integer", "minimum": 0}
            }),
            &["plan_id", "steps", "current"],
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _input: JsonValue,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Other(
            "update_plan must be intercepted at the loop level".into(),
        ))
    }
}
