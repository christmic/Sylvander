//! `ask_user` tool — marker tool the model invokes to ask the user a question.
//!
//! Never actually executed: the loop intercepts this tool name and
//! triggers the `AskUserGate` instead, which pauses the loop until
//! the user responds via the bus.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

#[derive(Default)]
pub struct AskUserTool;

impl AskUserTool {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &'static str {
        "ask_user"
    }

    fn description(&self) -> &'static str {
        "Pause and ask the user a clarifying question. Use this when you need \
         a decision, confirmation, or additional information. \
         Set `options` to constrain answers to a fixed set. Omit `options` \
         for free-text input. Set `multi_select: true` to allow multiple \
         options to be chosen."
    }

    fn input_schema(&self) -> InputSchema {
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
    }

    fn invocation_class(&self) -> crate::tool_invocation::ToolInvocationClass {
        crate::tool_invocation::ToolInvocationClass::Control
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _input: JsonValue,
    ) -> Result<ToolOutput, ToolError> {
        // Intercepted at the loop level — this should never run.
        Err(ToolError::Other(
            "ask_user must be intercepted at the loop level".into(),
        ))
    }
}
