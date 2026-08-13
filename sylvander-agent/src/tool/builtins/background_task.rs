//! `start_background_task` marker tool intercepted by the Agent loop.

use async_trait::async_trait;
use sylvander_llm_core::InputSchema;

use crate::execution::tool_context::ToolContext;
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};

#[derive(Default)]
pub struct StartBackgroundTaskTool;

impl StartBackgroundTaskTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ToolDefinition for StartBackgroundTaskTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::strict(
            "start_background_task",
            "Start an independent read-only background investigation and continue the main turn.",
            InputSchema::new_with_properties(
                serde_json::json!({
                    "purpose": {"type": "string", "description": "Short user-facing task label."},
                    "prompt": {"type": "string", "description": "Complete investigation request."}
                }),
                &["purpose", "prompt"],
            )
            .schema,
            crate::tool::invocation::ToolInvocationClass::Control,
        )
    }
}

#[async_trait]
impl ToolExecutor for StartBackgroundTaskTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Other(
            "start_background_task must be intercepted at the loop level".into(),
        ))
    }
}
