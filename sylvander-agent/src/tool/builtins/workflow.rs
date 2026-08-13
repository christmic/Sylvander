//! `manage_workflow` marker tool intercepted by the Agent loop.

use async_trait::async_trait;
use sylvander_llm_core::InputSchema;

use crate::execution::tool_context::ToolContext;
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};

#[derive(Default)]
pub struct ManageWorkflowTool;

impl ManageWorkflowTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ToolDefinition for ManageWorkflowTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::strict(
            "manage_workflow",
            "Create or advance your durable Runtime task. Runtime supplies identity, governance, and revision fencing.",
            InputSchema::from_json_value(serde_json::json!({
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "action": {"const": "create"},
                            "task_id": {"type": "string"},
                            "objective": {"type": "string"},
                            "token_budget": {"type": "integer", "minimum": 1},
                            "max_handoffs": {"type": "integer", "minimum": 0}
                        },
                        "required": ["action", "task_id", "objective", "token_budget", "max_handoffs"],
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "properties": {
                            "action": {"const": "transition"},
                            "task_id": {"type": "string"},
                            "state": {"type": "string", "enum": ["running", "blocked", "awaiting_review", "completed", "failed", "cancelled"]},
                            "consumed_tokens": {"type": "integer", "minimum": 0}
                        },
                        "required": ["action", "task_id", "state", "consumed_tokens"],
                        "additionalProperties": false
                    }
                ]
            }))
            .schema,
            crate::tool::invocation::ToolInvocationClass::Control,
        )
    }
}

#[async_trait]
impl ToolExecutor for ManageWorkflowTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Other(
            "manage_workflow must be intercepted at the loop level".into(),
        ))
    }
}
