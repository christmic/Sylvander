//! Structured workspace text search.

use async_trait::async_trait;
use serde_json::json;
use sylvander_llm_core::InputSchema;

use crate::execution::tool_context::{Cap, ToolContext};
use crate::execution::workspace::{
    MAX_QUERY_RESULTS, WorkspaceQueryLimits, WorkspaceSearchRequest,
};
#[cfg(test)]
use crate::tool::ToolTestExt as _;
use crate::tool::{
    PreparedToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, ToolSpec,
};

use super::list::{optional_string, parse_max_results, strict_object};
use super::workspace_error_output;

/// Search workspace text through the invocation's workspace executor.
#[derive(Debug, Clone, Copy, Default)]
pub struct SearchTool;

impl SearchTool {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ToolDefinition for SearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::strict(
            "Search",
            "Search UTF-8 workspace files without invoking a shell. Returns compact JSON matches with path, line number, line text, and an explicit truncated flag.",
            InputSchema::from_json_value(json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Literal text to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory path relative to the workspace (default: .)"
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_QUERY_RESULTS,
                    "description": "Maximum matches to return (default: 200, hard limit: 1000)"
                }
            },
            "required": ["query"],
            "additionalProperties": false
            }))
            .schema,
            crate::tool::invocation::ToolInvocationClass::Read,
        )
    }
}

#[async_trait]
impl ToolExecutor for SearchTool {
    async fn handle(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(Cap::Read) {
            return Ok(ToolOutput::err(
                "search capability not granted for this invocation",
            ));
        }
        let object = strict_object(call.input(), &["query", "path", "max_results"])?;
        let query = optional_string(object.get("query"), "query")?
            .filter(|query| !query.is_empty())
            .ok_or_else(|| ToolError::Other("missing non-empty required field `query`".into()))?;
        let path = optional_string(object.get("path"), "path")?.unwrap_or(".");
        let max_results = parse_max_results(object.get("max_results"))?;
        let limits = WorkspaceQueryLimits {
            max_results,
            ..WorkspaceQueryLimits::default()
        };
        let target = match ctx.require_execution_target() {
            Ok(target) => target,
            Err(error) => return Ok(workspace_error_output(error)),
        };
        let result = match ctx
            .executor
            .search(
                target,
                WorkspaceSearchRequest {
                    relative_path: path.into(),
                    query: query.into(),
                    limits,
                },
            )
            .await
        {
            Ok(result) => result,
            Err(error) => return Ok(workspace_error_output(error)),
        };
        let matches = result
            .matches
            .into_iter()
            .map(|matched| {
                json!({
                    "path": matched.relative_path,
                    "line_number": matched.line_number,
                    "line": matched.line,
                })
            })
            .collect::<Vec<_>>();
        Ok(ToolOutput::ok(
            serde_json::to_string(&json!({
                "matches": matches,
                "truncated": result.truncated,
            }))
            .expect("workspace search result is serializable"),
        ))
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/tools_search.rs"]
mod tests;
