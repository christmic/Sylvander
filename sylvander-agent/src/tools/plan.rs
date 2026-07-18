//! `present_plan` marker tool intercepted by the Agent loop.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

#[derive(Default)]
pub struct PresentPlanTool;

impl PresentPlanTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for PresentPlanTool {
    fn name(&self) -> &'static str {
        "present_plan"
    }

    fn description(&self) -> &'static str {
        "Present an ordered implementation plan for explicit user review before proceeding."
    }

    fn input_schema(&self) -> InputSchema {
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
    }

    fn invocation_class(&self) -> crate::tool_invocation::ToolInvocationClass {
        crate::tool_invocation::ToolInvocationClass::Control
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _input: JsonValue,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Other(
            "present_plan must be intercepted at the loop level".into(),
        ))
    }
}
