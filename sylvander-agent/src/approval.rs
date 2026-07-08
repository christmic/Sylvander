//! Tool approval — gate mechanism for controlling tool execution.
//!
//! The [`ApprovalGate`] trait is passed into [`AgentLoop`](crate::loop_::AgentLoop).
//! Before executing tools, the loop calls `check_batch().await` — the
//! loop PAUSES here until the gate returns.

use async_trait::async_trait;
use serde_json::Value as JsonValue;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A single tool call request presented for approval.
#[derive(Debug, Clone)]
pub struct ToolUseRequest {
    /// Tool call ID (matches `tool_use.id`).
    pub call_id: String,
    /// Tool name.
    pub tool_name: String,
    /// Parsed input arguments.
    pub input: JsonValue,
}

/// Result of approving a batch of tool calls.
#[derive(Debug, Clone)]
pub struct ApprovalBatchResult {
    /// One decision per tool, in the same order as the request.
    pub decisions: Vec<ApprovalDecision>,
}

/// Decision for a single tool call.
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    /// Execute the tool.
    Approved,
    /// Skip — loop will emit a rejection with this reason.
    Rejected { reason: String },
}

// ---------------------------------------------------------------------------
// ApprovalGate trait
// ---------------------------------------------------------------------------

/// The approval mechanism — called by the loop before executing tools.
///
/// The loop PAUSES here. It does not continue until `check_batch`
/// returns. The implementation decides whether to approve immediately
/// (rule-based) or wait for external input (bus-based).
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Check a batch of tool calls. Returns one decision per tool.
    async fn check_batch(
        &self,
        tools: &[ToolUseRequest],
    ) -> ApprovalBatchResult;
}
