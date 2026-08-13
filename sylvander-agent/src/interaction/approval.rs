//! Tool approval — gate mechanism for controlling tool execution.
//!
//! The [`ApprovalGate`] trait is passed into
//! [`AgentLoop`](crate::kernel::agent_loop::AgentLoop).
//! Before executing tools, the loop calls `check_batch().await` — the
//! loop PAUSES here until the gate returns.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value as JsonValue;

use crate::tool::invocation::ToolInvocationClass;
use crate::tool::{
    PreparedToolCall, SandboxRequirement, ToolExecutionMode, ToolExecutionPolicy,
    ToolFilesystemPolicy, ToolNetworkPolicy,
};

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
    /// Trusted input-specific scheduling and environment facts.
    pub facts: ToolApprovalFacts,
}

impl ToolUseRequest {
    /// Build an approval request from the exact immutable call that may run.
    #[must_use]
    pub fn from_prepared(call_id: &str, call: &PreparedToolCall) -> Self {
        Self {
            call_id: call_id.to_owned(),
            tool_name: call.spec().name.clone(),
            input: call.input().clone(),
            facts: ToolApprovalFacts::from_prepared(call),
        }
    }
}

/// Trusted input-specific facts presented to approval policy and UI adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolApprovalFacts {
    /// Stable capability class declared by the selected tool route.
    pub invocation_class: ToolInvocationClass,
    /// Input-specific batch coordination requirement.
    pub execution_mode: ToolExecutionMode,
    /// Input-specific physical environment policy.
    pub execution_policy: ToolExecutionPolicy,
}

impl ToolApprovalFacts {
    /// Construct trusted facts in a Runtime approval-policy test or adapter.
    #[must_use]
    pub const fn new(
        invocation_class: ToolInvocationClass,
        execution_mode: ToolExecutionMode,
        execution_policy: ToolExecutionPolicy,
    ) -> Self {
        Self {
            invocation_class,
            execution_mode,
            execution_policy,
        }
    }

    fn from_prepared(call: &PreparedToolCall) -> Self {
        Self::new(
            call.spec().invocation_class,
            call.execution_mode(),
            call.execution_policy().clone(),
        )
    }

    /// Stable material included in persistent approval fingerprints.
    #[must_use]
    pub fn fingerprint_material(&self) -> JsonValue {
        serde_json::json!({
            "invocation_class": invocation_class_name(self.invocation_class),
            "execution_mode": match self.execution_mode {
                ToolExecutionMode::Parallel => "parallel",
                ToolExecutionMode::Exclusive => "exclusive",
            },
            "sandbox": match self.execution_policy.sandbox {
                SandboxRequirement::NotApplicable => "not_applicable",
                SandboxRequirement::Preferred => "preferred",
                SandboxRequirement::Required => "required",
            },
            "filesystem": match self.execution_policy.filesystem {
                ToolFilesystemPolicy::None => "none",
                ToolFilesystemPolicy::WorkspaceRead => "workspace_read",
                ToolFilesystemPolicy::WorkspaceWrite => "workspace_write",
            },
            "network": match self.execution_policy.network {
                ToolNetworkPolicy::Denied => "denied",
                ToolNetworkPolicy::FullAfterApproval => "full_after_approval",
            },
            "launches_processes": self.execution_policy.launches_processes,
        })
    }
}

const fn invocation_class_name(class: ToolInvocationClass) -> &'static str {
    match class {
        ToolInvocationClass::Read => "read",
        ToolInvocationClass::FilesystemMutation => "filesystem_mutation",
        ToolInvocationClass::Terminal => "terminal",
        ToolInvocationClass::Browser => "browser",
        ToolInvocationClass::HostControl => "host_control",
        ToolInvocationClass::ArbitraryMcp => "arbitrary_mcp",
        ToolInvocationClass::MemoryCandidate => "memory_candidate",
        ToolInvocationClass::Control => "control",
        ToolInvocationClass::Extension => "extension",
    }
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
    async fn check_batch(&self, tools: &[ToolUseRequest]) -> ApprovalBatchResult;
}

// ---------------------------------------------------------------------------
// Rule-based approval
// ---------------------------------------------------------------------------

/// A static approval rule — auto-approve or auto-reject matching tools.
#[derive(Debug, Clone)]
pub struct ApprovalRule {
    /// Tool names this rule applies to.
    pub tools: Vec<String>,
    /// What to do.
    pub action: RuleAction,
}

/// Action for a matching rule.
#[derive(Debug, Clone)]
pub enum RuleAction {
    /// Auto-approve (skip bus round-trip).
    AutoApprove,
    /// Auto-reject (skip bus round-trip, no execution).
    AutoReject { reason: String },
}

/// Composite gate: applies rules first, falls back to another gate for
/// tools that don't match any rule.
pub struct RuleBasedApprovalGate {
    rules: Vec<ApprovalRule>,
    fallback: Arc<dyn ApprovalGate>,
}

impl RuleBasedApprovalGate {
    /// Create a new composite gate.
    pub fn new(rules: Vec<ApprovalRule>, fallback: Arc<dyn ApprovalGate>) -> Self {
        Self { rules, fallback }
    }

    fn match_rule(&self, tool_name: &str) -> Option<RuleAction> {
        for rule in &self.rules {
            if rule.tools.iter().any(|t| t == tool_name) {
                return Some(rule.action.clone());
            }
        }
        None
    }
}

#[async_trait]
impl ApprovalGate for RuleBasedApprovalGate {
    async fn check_batch(&self, tools: &[ToolUseRequest]) -> ApprovalBatchResult {
        let mut decisions: Vec<Option<ApprovalDecision>> = vec![None; tools.len()];
        let mut needs_fallback: Vec<usize> = Vec::new();

        // Apply rules
        for (i, tool) in tools.iter().enumerate() {
            if let Some(action) = self.match_rule(&tool.tool_name) {
                decisions[i] = Some(match action {
                    RuleAction::AutoApprove => ApprovalDecision::Approved,
                    RuleAction::AutoReject { reason } => ApprovalDecision::Rejected { reason },
                });
            } else {
                needs_fallback.push(i);
            }
        }

        // Delegate remaining to fallback gate
        if !needs_fallback.is_empty() {
            let remaining: Vec<ToolUseRequest> =
                needs_fallback.iter().map(|&i| tools[i].clone()).collect();
            let result = self.fallback.check_batch(&remaining).await;
            for (j, &i) in needs_fallback.iter().enumerate() {
                decisions[i] = Some(result.decisions[j].clone());
            }
        }

        ApprovalBatchResult {
            decisions: decisions.into_iter().map(|d| d.unwrap()).collect(),
        }
    }
}
