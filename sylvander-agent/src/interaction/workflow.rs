//! Neutral Agent intent boundary for Runtime-owned durable workflow.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Agent-authored workflow intent. Runtime supplies identity, revision fences,
/// persistence, and governance facts rather than trusting model arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowCommand {
    Create {
        task_id: String,
        objective: String,
        token_budget: u64,
        max_handoffs: u32,
    },
    Transition {
        task_id: String,
        state: WorkflowTaskState,
        consumed_tokens: u64,
    },
}

/// Agent-visible subset of durable task states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTaskState {
    Running,
    Blocked,
    AwaitingReview,
    Completed,
    Failed,
    Cancelled,
}

/// Content-safe durable receipt returned to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowReceipt {
    pub task_id: String,
    pub state: WorkflowTaskState,
    pub revision: u64,
}

#[async_trait]
pub trait WorkflowGate: Send + Sync {
    async fn apply(&self, command: WorkflowCommand) -> Result<WorkflowReceipt, String>;
}
