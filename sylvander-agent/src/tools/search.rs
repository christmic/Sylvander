//! Structured workspace text search.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use sylvander_llm_anthropic::api::types::InputSchema;

use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::{Cap, ToolContext};
use crate::workspace_executor::{MAX_QUERY_RESULTS, WorkspaceQueryLimits, WorkspaceSearchRequest};

use super::list::{optional_string, parse_max_results, strict_object};

/// Search workspace text through the invocation's workspace executor.
#[derive(Debug, Clone)]
pub struct SearchTool {
    workdir: PathBuf,
}

impl SearchTool {
    #[must_use]
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: workdir.into(),
        }
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "Search"
    }

    fn description(&self) -> &'static str {
        "Search UTF-8 workspace files without invoking a shell. Returns compact JSON matches with path, line number, line text, and an explicit truncated flag."
    }

    fn input_schema(&self) -> InputSchema {
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
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        if !ctx.has_cap(Cap::Read) {
            return Ok(ToolOutput::err(
                "search capability not granted for this invocation",
            ));
        }
        let object = strict_object(&input, &["query", "path", "max_results"])?;
        let query = optional_string(object.get("query"), "query")?
            .filter(|query| !query.is_empty())
            .ok_or_else(|| ToolError::Other("missing non-empty required field `query`".into()))?;
        let path = optional_string(object.get("path"), "path")?.unwrap_or(".");
        let max_results = parse_max_results(object.get("max_results"))?;
        let limits = WorkspaceQueryLimits {
            max_results,
            ..WorkspaceQueryLimits::default()
        };
        let target = ctx.execution_target_for(&self.workdir);
        let result = match ctx
            .executor
            .search(
                &target,
                WorkspaceSearchRequest {
                    relative_path: path.into(),
                    query: query.into(),
                    limits,
                },
            )
            .await
        {
            Ok(result) => result,
            Err(error) => return Ok(ToolOutput::err(error.to_string())),
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
mod tests {
    use super::*;
    use sylvander_protocol::SessionContext;

    fn context(root: &std::path::Path) -> ToolContext {
        ToolContext::new(SessionContext::new("user", "agent", "session"))
            .with_fs_root(root)
            .with_capability(Cap::Read)
    }

    #[tokio::test]
    async fn searches_with_structured_results_and_explicit_truncation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("one.txt"), "crab one\ncrab two\n").unwrap();
        let output = SearchTool::new("/")
            .execute(
                &context(dir.path()),
                json!({"query": "crab", "max_results": 1}),
            )
            .await
            .unwrap();
        assert!(!output.is_error);
        let value: JsonValue = serde_json::from_str(&output.content).unwrap();
        assert_eq!(value["matches"].as_array().unwrap().len(), 1);
        assert_eq!(value["matches"][0]["line_number"], 1);
        assert_eq!(value["truncated"], true);
    }

    #[tokio::test]
    async fn rejects_unknown_or_empty_input() {
        let dir = tempfile::tempdir().unwrap();
        let tool = SearchTool::new("/");
        assert!(
            tool.execute(&context(dir.path()), json!({"query": "", "glob": "*"}))
                .await
                .is_err()
        );
        assert!(
            tool.execute(&context(dir.path()), json!({"query": ""}))
                .await
                .is_err()
        );
    }

    #[test]
    fn schema_is_strict_and_bounded() {
        let schema = SearchTool::new("/").input_schema();
        assert_eq!(schema.schema["additionalProperties"], false);
        assert_eq!(schema.schema["required"], json!(["query"]));
        assert_eq!(
            schema.schema["properties"]["max_results"]["maximum"],
            MAX_QUERY_RESULTS
        );
    }
}
