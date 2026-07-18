//! `start_background_task` marker tool intercepted by the Agent loop.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

#[derive(Default)]
pub struct StartBackgroundTaskTool;

impl StartBackgroundTaskTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for StartBackgroundTaskTool {
    fn name(&self) -> &'static str {
        "start_background_task"
    }

    fn description(&self) -> &'static str {
        "Start an independent read-only background investigation and continue the main turn."
    }

    fn input_schema(&self) -> InputSchema {
        InputSchema::new_with_properties(
            serde_json::json!({
                "purpose": {"type": "string", "description": "Short user-facing task label."},
                "prompt": {"type": "string", "description": "Complete investigation request."}
            }),
            &["purpose", "prompt"],
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
            "start_background_task must be intercepted at the loop level".into(),
        ))
    }
}
