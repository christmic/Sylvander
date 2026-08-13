//! `ask_user` tool — marker tool the model invokes to ask the user a question.
//!
//! Never actually executed: the loop intercepts this tool name and
//! triggers the `AskUserGate` instead, which pauses the loop until
//! the user responds via the bus.

use async_trait::async_trait;
use sylvander_llm_core::InputSchema;

use crate::execution::tool_context::ToolContext;
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};

#[derive(Default)]
pub struct AskUserTool;

impl AskUserTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl ToolDefinition for AskUserTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::strict(
            "ask_user",
            "Pause and ask the user a clarifying question. Use this when you need \
         a decision, confirmation, or additional information. \
         Set `options` to constrain answers to a fixed set. Omit `options` \
         for free-text input. Set `multi_select: true` to allow multiple \
         options to be chosen.",
            InputSchema::new_with_properties(
                serde_json::json!({
                    "question": {
                        "type": "string",
                        "description": "The question to ask the user."
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of choices. Omit for free-text input."
                    },
                    "multi_select": {
                        "type": "boolean",
                        "description": "If true, allow selecting multiple options. Default false."
                    }
                }),
                &["question"],
            )
            .schema,
            crate::tool::invocation::ToolInvocationClass::Control,
        )
    }
}

#[async_trait]
impl ToolExecutor for AskUserTool {
    async fn handle(
        &self,
        _ctx: &ToolContext,
        _call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        // Intercepted at the loop level — this should never run.
        Err(ToolError::Other(
            "ask_user must be intercepted at the loop level".into(),
        ))
    }
}
