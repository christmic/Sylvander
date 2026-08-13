//! Immutable service-port snapshot for one Agent execution.
//!
//! [`crate::turn::request::AgentTurnRequest`] contains turn data;
//! this module contains the Runtime-selected implementations used to perform
//! that data. Keeping them separate prevents a request from becoming a service
//! locator and makes it impossible to deserialize client input into executable
//! authority.

use std::sync::Arc;

use sylvander_llm_core::ModelProvider;

use crate::execution::artifact::TurnArtifactStore;
use crate::execution::tool_context::ToolContext;
use crate::interaction::approval::ApprovalGate;
use crate::interaction::ask_user::AskUserGate;
use crate::interaction::background_task::TaskGate;
use crate::interaction::plan::PlanGate;
use crate::tool::invocation::{ToolInvocationGateway, ToolInvocationSnapshot};
use crate::turn::error::AgentLoopError;
use crate::turn::request::AgentTurnRequest;

/// Runtime-selected service implementations pinned for one Agent turn.
///
/// The snapshot is intentionally not serializable. Runtime constructs it only
/// after resolving credentials, execution targets, authorization policy, and
/// interactive decision channels.
#[derive(Clone)]
pub struct AgentExecutionPorts {
    /// Exact provider-neutral model route selected by Runtime.
    pub(crate) model: Arc<dyn ModelProvider>,
    /// Prepared tool environment containing injected filesystem/process ports.
    pub(crate) tool_context: ToolContext,
    /// Central authorization and audit boundary for executable tools.
    pub(crate) invocation_gateway: Arc<dyn ToolInvocationGateway>,
    /// Exact executable and prompt-context capability revision for this turn.
    pub(crate) invocation_snapshot: ToolInvocationSnapshot,
    /// Optional interactive approval port.
    pub(crate) approval_gate: Option<Arc<dyn ApprovalGate>>,
    /// Optional structured user-question port.
    pub(crate) ask_user_gate: Option<Arc<dyn AskUserGate>>,
    /// Optional plan-review port.
    pub(crate) plan_gate: Option<Arc<dyn PlanGate>>,
    /// Optional background-investigation port.
    pub(crate) task_gate: Option<Arc<dyn TaskGate>>,
    /// Optional immutable artifact authority bound to this exact turn.
    pub(crate) artifact_store: Option<Arc<dyn TurnArtifactStore>>,
}

impl AgentExecutionPorts {
    /// Create the minimal explicit port snapshot required to execute a turn.
    #[must_use]
    pub fn new(
        model: Arc<dyn ModelProvider>,
        tool_context: ToolContext,
        invocation_gateway: Arc<dyn ToolInvocationGateway>,
        invocation_snapshot: ToolInvocationSnapshot,
    ) -> Self {
        Self {
            model,
            tool_context,
            invocation_gateway,
            invocation_snapshot,
            approval_gate: None,
            ask_user_gate: None,
            plan_gate: None,
            task_gate: None,
            artifact_store: None,
        }
    }

    /// Borrow the exact provider-neutral model route pinned for this turn.
    #[must_use]
    pub fn model(&self) -> &Arc<dyn ModelProvider> {
        &self.model
    }

    /// Attach the Runtime-owned interactive approval port.
    #[must_use]
    pub fn with_approval_gate(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approval_gate = Some(gate);
        self
    }

    /// Attach the Runtime-owned structured user-question port.
    #[must_use]
    pub fn with_ask_user_gate(mut self, gate: Arc<dyn AskUserGate>) -> Self {
        self.ask_user_gate = Some(gate);
        self
    }

    /// Attach the Runtime-owned plan-review port.
    #[must_use]
    pub fn with_plan_gate(mut self, gate: Arc<dyn PlanGate>) -> Self {
        self.plan_gate = Some(gate);
        self
    }

    /// Attach the Runtime-owned background-investigation port.
    #[must_use]
    pub fn with_task_gate(mut self, gate: Arc<dyn TaskGate>) -> Self {
        self.task_gate = Some(gate);
        self
    }

    /// Attach Runtime's turn-bound artifact retention authority.
    #[must_use]
    pub fn with_artifact_store(mut self, store: Arc<dyn TurnArtifactStore>) -> Self {
        self.artifact_store = Some(store);
        self
    }

    /// Borrow the optional turn-bound artifact authority.
    #[must_use]
    pub fn artifact_store(&self) -> Option<&dyn TurnArtifactStore> {
        self.artifact_store.as_deref()
    }

    /// Verify that data and executable authority describe the same turn.
    ///
    /// Runtime may build both values through different application services.
    /// This fail-closed check prevents identity or workspace drift before any
    /// hook, provider request, authorization decision, or tool call occurs.
    pub fn validate_for(&self, request: &AgentTurnRequest) -> Result<(), AgentLoopError> {
        if self.tool_context.execution.as_ref() != &request.execution {
            return Err(AgentLoopError::Validation(
                "turn request and execution ports carry different authority".into(),
            ));
        }

        let requested_tools = request.tools.invocation_descriptors();
        if requested_tools
            .iter()
            .any(|tool| !self.invocation_snapshot.authorizes(&tool.name, tool.class))
            || !self
                .invocation_gateway
                .snapshot()
                .has_same_executable_surface(&self.invocation_snapshot)
        {
            return Err(AgentLoopError::Validation(
                "turn tools and invocation gateway expose different executable surfaces".into(),
            ));
        }
        Ok(())
    }
}

impl std::fmt::Debug for AgentExecutionPorts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentExecutionPorts")
            .field("tool_context", &self.tool_context)
            .field("invocation_snapshot", &self.invocation_snapshot)
            .field("approval_gate", &self.approval_gate.is_some())
            .field("ask_user_gate", &self.ask_user_gate.is_some())
            .field("plan_gate", &self.plan_gate.is_some())
            .field("task_gate", &self.task_gate.is_some())
            .field("artifact_store", &self.artifact_store.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "../../tests/unit/execution_ports.rs"]
mod tests;
