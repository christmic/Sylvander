//! Runtime-owned interactive decision gates for one Agent turn.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, oneshot};

use sylvander_agent::approval::{
    ApprovalBatchResult, ApprovalDecision, ApprovalGate, ToolUseRequest,
};
use sylvander_agent::ask_user_gate::AskUserGate;
use sylvander_agent::plan_gate::{PlanDecision, PlanGate};
use sylvander_api::{
    AgentId, AgentInstanceId, BusMessage, Recipient, SessionId, StreamEvent, ToolCallInfo,
};
use sylvander_channel::MessageBus;

use crate::agent::approval::{ApprovalGrantContext, ApprovalGrantKey, ApprovalMemory};

const APPROVAL_TIMEOUT_SECS: u64 = 2 * 60;
const USER_RESPONSE_TIMEOUT_SECS: u64 = 5 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct InteractionKey {
    instance: AgentInstanceId,
    session: SessionId,
    subject: String,
}

impl InteractionKey {
    pub(super) fn new(
        agent_instance_id: AgentInstanceId,
        session_id: SessionId,
        subject_id: impl Into<String>,
    ) -> Self {
        Self {
            instance: agent_instance_id,
            session: session_id,
            subject: subject_id.into(),
        }
    }
}

fn instance_stream_event(
    session_id: SessionId,
    agent_id: AgentId,
    agent_instance_id: AgentInstanceId,
    event: StreamEvent,
) -> BusMessage {
    let mut message = BusMessage::stream_event(session_id, agent_id.clone(), event);
    message.recipient = Recipient::AgentInstance {
        instance_id: agent_instance_id,
        agent_id,
    };
    message
}

pub(super) struct PendingApproval {
    pub(super) session_id: SessionId,
    pub(super) grant: ApprovalGrantKey,
    pub(super) persistent_identity_authorized: bool,
    pub(super) allowed_scopes: Vec<sylvander_api::ApprovalScope>,
    pub(super) sender: oneshot::Sender<ApprovalDecision>,
}

pub(super) struct PendingAnswer {
    pub(super) session_id: SessionId,
    pub(super) sender: oneshot::Sender<Vec<String>>,
}

pub(super) struct PendingPlan {
    pub(super) session_id: SessionId,
    pub(super) sender: oneshot::Sender<PlanDecision>,
}

/// Approval gate that publishes requests and waits for Runtime-authorized decisions.
pub(super) struct BusApprovalGate {
    pub(super) bus: Arc<dyn MessageBus>,
    pub(super) agent_id: AgentId,
    pub(super) agent_instance_id: AgentInstanceId,
    pub(super) session_id: SessionId,
    pub(super) grant_context: ApprovalGrantContext,
    pub(super) persistent_identity_authorized: bool,
    pub(super) pending_approvals: Arc<Mutex<HashMap<InteractionKey, PendingApproval>>>,
    pub(super) approval_memory: Arc<Mutex<ApprovalMemory>>,
}

pub(super) struct DenyAllApprovalGate;

#[async_trait::async_trait]
impl ApprovalGate for DenyAllApprovalGate {
    async fn check_batch(&self, tools: &[ToolUseRequest]) -> ApprovalBatchResult {
        ApprovalBatchResult {
            decisions: tools
                .iter()
                .map(|_| ApprovalDecision::Rejected {
                    reason: "tool execution denied by runtime permission policy".into(),
                })
                .collect(),
        }
    }
}

#[async_trait::async_trait]
impl ApprovalGate for BusApprovalGate {
    async fn check_batch(&self, tools: &[ToolUseRequest]) -> ApprovalBatchResult {
        let batch_id = uuid::Uuid::new_v4().to_string();
        let mut decisions = vec![None; tools.len()];
        let mut receivers = Vec::new();
        let allowed_scopes = self
            .approval_memory
            .lock()
            .await
            .allowed_scopes(self.persistent_identity_authorized);
        let mut requested_tools = Vec::new();

        for (index, tool) in tools.iter().enumerate() {
            let grant = self.grant_context.key_for(tool);
            if self
                .approval_memory
                .lock()
                .await
                .contains(&self.session_id, &grant)
                .await
            {
                decisions[index] = Some(ApprovalDecision::Approved);
                continue;
            }
            let (tx, rx) = oneshot::channel();
            self.pending_approvals.lock().await.insert(
                InteractionKey::new(
                    self.agent_instance_id.clone(),
                    self.session_id.clone(),
                    tool.call_id.clone(),
                ),
                PendingApproval {
                    session_id: self.session_id.clone(),
                    grant,
                    persistent_identity_authorized: self.persistent_identity_authorized,
                    allowed_scopes: allowed_scopes.clone(),
                    sender: tx,
                },
            );
            receivers.push((index, tool.call_id.clone(), rx));
            requested_tools.push(tool);
        }

        if !requested_tools.is_empty() {
            let _ = self
                .bus
                .publish(instance_stream_event(
                    self.session_id.clone(),
                    self.agent_id.clone(),
                    self.agent_instance_id.clone(),
                    StreamEvent::ToolApprovalRequired {
                        batch_id,
                        tools: requested_tools
                            .into_iter()
                            .map(|tool| ToolCallInfo {
                                call_id: tool.call_id.clone(),
                                tool_name: tool.tool_name.clone(),
                                input: tool.input.clone(),
                            })
                            .collect(),
                        allowed_scopes,
                    },
                ))
                .await;
        }

        for (index, call_id, rx) in receivers {
            let decision = if let Ok(Ok(decision)) =
                tokio::time::timeout(Duration::from_secs(APPROVAL_TIMEOUT_SECS), rx).await
            {
                decision
            } else {
                publish_interaction_timeout(
                    &self.bus,
                    &self.session_id,
                    &self.agent_id,
                    sylvander_api::InteractionTimeoutKind::Approval,
                    &call_id,
                    APPROVAL_TIMEOUT_SECS,
                    sylvander_api::TimeoutRecovery::RetryRequest,
                )
                .await;
                ApprovalDecision::Rejected {
                    reason: "approval timeout".into(),
                }
            };
            decisions[index] = Some(decision);
            self.pending_approvals
                .lock()
                .await
                .remove(&InteractionKey::new(
                    self.agent_instance_id.clone(),
                    self.session_id.clone(),
                    call_id,
                ));
        }
        ApprovalBatchResult {
            decisions: decisions
                .into_iter()
                .map(|decision| decision.expect("every approval decision must settle"))
                .collect(),
        }
    }
}

pub(super) struct BusAskUserGate {
    pub(super) bus: Arc<dyn MessageBus>,
    pub(super) agent_id: AgentId,
    pub(super) agent_instance_id: AgentInstanceId,
    pub(super) session_id: SessionId,
    pub(super) pending_answers: Arc<Mutex<HashMap<InteractionKey, PendingAnswer>>>,
}

#[async_trait::async_trait]
impl AskUserGate for BusAskUserGate {
    async fn ask(
        &self,
        call_id: &str,
        question: &str,
        options: Vec<String>,
        multi_select: bool,
    ) -> Vec<String> {
        let (tx, rx) = oneshot::channel();
        self.pending_answers.lock().await.insert(
            InteractionKey::new(
                self.agent_instance_id.clone(),
                self.session_id.clone(),
                call_id,
            ),
            PendingAnswer {
                session_id: self.session_id.clone(),
                sender: tx,
            },
        );
        let _ = self
            .bus
            .publish(instance_stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                self.agent_instance_id.clone(),
                StreamEvent::AskUser {
                    call_id: call_id.into(),
                    question: question.into(),
                    options,
                    multi_select,
                },
            ))
            .await;

        let answer = if let Ok(Ok(answer)) =
            tokio::time::timeout(Duration::from_secs(USER_RESPONSE_TIMEOUT_SECS), rx).await
        {
            answer
        } else {
            publish_interaction_timeout(
                &self.bus,
                &self.session_id,
                &self.agent_id,
                sylvander_api::InteractionTimeoutKind::Question,
                call_id,
                USER_RESPONSE_TIMEOUT_SECS,
                sylvander_api::TimeoutRecovery::RetryRequest,
            )
            .await;
            Vec::new()
        };
        self.pending_answers
            .lock()
            .await
            .remove(&InteractionKey::new(
                self.agent_instance_id.clone(),
                self.session_id.clone(),
                call_id,
            ));
        answer
    }
}

pub(super) struct BusPlanGate {
    pub(super) bus: Arc<dyn MessageBus>,
    pub(super) agent_id: AgentId,
    pub(super) agent_instance_id: AgentInstanceId,
    pub(super) session_id: SessionId,
    pub(super) pending_plans: Arc<Mutex<HashMap<InteractionKey, PendingPlan>>>,
}

#[async_trait::async_trait]
impl PlanGate for BusPlanGate {
    async fn review(&self, plan_id: &str, steps: Vec<String>) -> PlanDecision {
        let (tx, rx) = oneshot::channel();
        self.pending_plans.lock().await.insert(
            InteractionKey::new(
                self.agent_instance_id.clone(),
                self.session_id.clone(),
                plan_id,
            ),
            PendingPlan {
                session_id: self.session_id.clone(),
                sender: tx,
            },
        );
        let _ = self
            .bus
            .publish(instance_stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                self.agent_instance_id.clone(),
                StreamEvent::PlanProposed {
                    plan_id: plan_id.into(),
                    steps,
                    current: 0,
                },
            ))
            .await;

        let decision = if let Ok(Ok(decision)) =
            tokio::time::timeout(Duration::from_secs(USER_RESPONSE_TIMEOUT_SECS), rx).await
        {
            decision
        } else {
            publish_interaction_timeout(
                &self.bus,
                &self.session_id,
                &self.agent_id,
                sylvander_api::InteractionTimeoutKind::Plan,
                plan_id,
                USER_RESPONSE_TIMEOUT_SECS,
                sylvander_api::TimeoutRecovery::RetryRequest,
            )
            .await;
            PlanDecision::Rejected {
                reason: "plan review timed out".into(),
            }
        };
        self.pending_plans.lock().await.remove(&InteractionKey::new(
            self.agent_instance_id.clone(),
            self.session_id.clone(),
            plan_id,
        ));
        decision
    }

    async fn update(&self, plan_id: &str, steps: Vec<String>, current: usize) {
        let _ = self
            .bus
            .publish(BusMessage::stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                StreamEvent::PlanUpdated {
                    plan_id: plan_id.into(),
                    steps,
                    current,
                },
            ))
            .await;
    }
}

pub(super) async fn publish_interaction_timeout(
    bus: &Arc<dyn MessageBus>,
    session_id: &SessionId,
    agent_id: &AgentId,
    kind: sylvander_api::InteractionTimeoutKind,
    subject_id: &str,
    timeout_secs: u64,
    recovery: sylvander_api::TimeoutRecovery,
) {
    let _ = bus
        .publish(BusMessage::stream_event(
            session_id.clone(),
            agent_id.clone(),
            StreamEvent::InteractionTimedOut {
                kind,
                subject_id: subject_id.into(),
                timeout_secs,
                recovery,
            },
        ))
        .await;
}

pub(super) fn normalize_rejection_reason(reason: Option<&str>) -> String {
    reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map_or_else(
            || "rejected by user".into(),
            |reason| reason.chars().take(500).collect(),
        )
}
