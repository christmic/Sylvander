use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::instance::{
    AgentDefinitionKey, AgentInstanceOrigin, ApprovalRoute, HistoryView, SessionAgentRole,
};
use crate::coordination::arbitration::{
    ArbitrationCase, ArbitrationState, ModeratorDecision, ModeratorVerdict,
};
use crate::coordination::governance::{
    GovernanceFinding, GovernancePolicy, ProgressObservation, WaitDependency,
};
use crate::coordination::handoff::{HandoffState, TaskHandoff};
use crate::coordination::mailbox::{
    CoordinationMessage, CoordinationMessageKind, MessageDeliveryState,
};
use crate::coordination::service::{
    ClaimTaskRequest, CoordinationService, CreateTaskRequest, DefineAgentOutcome,
    DefineAgentRequest, DispatchMessageOutcome, DispatchMessageRequest, FinishClaimedTaskRequest,
    ForkAgentOutcome, ForkAgentRequest, ProposeHandoffRequest, RelateAgentsOutcome,
    RelateAgentsRequest, ReportWaitRequest,
};
use crate::coordination::task::{CoordinationTask, CoordinationTaskState, TaskDependency};
use crate::coordination::topology::{AgentRelation, AgentRelationKind, SessionTopology};
use crate::coordination::workspace::{
    AgentWorkspaceView, WorkspaceAccess, WorkspaceIsolation, WorkspaceViewState,
};
use crate::session::SessionMetadata;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::{MessageRole, SessionLifetime, SessionStore, StoredSession};
use crate::storage::workspace_coordination::AgentWorkspaceStore;
use sylvander_api::{
    AgentId, AgentInstanceId, CoordinationMessageId, GovernanceCaseId, HandoffId, SwarmId, TaskId,
    WorkspaceViewId,
};
use sylvander_benchmark_runtime::{
    FailurePoint, FaultController, FaultDecision, FaultInjectionSpec,
};

use super::*;

fn stored_session() -> StoredSession {
    let mut session = StoredSession::new(
        SessionId::new("multi-session"),
        "multi-session",
        SessionLifetime::Persistent,
        SessionMetadata {
            workspace: PathBuf::from("/tmp/project"),
            name: "multi-session".into(),
            user_id: "user-1".into(),
        },
        vec![AgentId::new("orchestrator")],
    );
    session.effective_config = Some(effective_config("orchestrator"));
    session
}

fn effective_config(agent_id: &str) -> sylvander_api::SessionEffectiveConfig {
    let source = sylvander_api::SessionConfigSource {
        kind: sylvander_api::SessionConfigSourceKind::AgentDefault,
        reference: Some(format!("{agent_id}@7")),
    };
    sylvander_api::SessionEffectiveConfig {
        agent_id: AgentId::new(agent_id),
        agent_revision: 7,
        provider_id: "primary".into(),
        provider_revision: 1,
        model_id: "model-a".into(),
        model_revision: 1,
        reasoning_effort: sylvander_api::ReasoningEffort::Medium,
        permissions: sylvander_api::PermissionProfile::default(),
        prompt_profile: None,
        system_prompt_sha256: "sha256:test".into(),
        prompt_manifest: sylvander_api::PromptManifest {
            layers: Vec::new(),
            aggregate_sha256: "sha256:manifest".into(),
            total_bytes: 0,
        },
        agent_workspace: None,
        user_workspace: None,
        workspace_mounts: Vec::new(),
        execution_target: "local".into(),
        provenance: sylvander_api::SessionConfigProvenance {
            model: source.clone(),
            reasoning_effort: source.clone(),
            permissions: source.clone(),
            prompt_profile: source.clone(),
            system_prompt: source.clone(),
            agent_workspace: source.clone(),
            user_workspace: source.clone(),
            execution_target: source,
        },
    }
}

async fn bind_test_config(store: &SqliteSessionStore, instance_id: &str, agent_id: &str) {
    store
        .save_agent_instance_config(
            &AgentInstanceConfig {
                session_id: SessionId::new("multi-session"),
                instance_id: AgentInstanceId::new(instance_id),
                config_revision: 0,
                effective: effective_config(agent_id),
                updated_at: 10,
            },
            None,
        )
        .await
        .unwrap();
}

fn instance(id: &str, agent: &str, role: SessionAgentRole) -> AgentInstance {
    AgentInstance {
        instance_id: AgentInstanceId::new(id),
        session_id: SessionId::new("multi-session"),
        definition: AgentDefinitionKey {
            agent_id: AgentId::new(agent),
            revision: 7,
        },
        origin: AgentInstanceOrigin::Defined,
        role,
        history_view: HistoryView::SharedLane { cursor: 0 },
        approval_route: ApprovalRoute::User,
        state: AgentInstanceState::Ready,
        lifecycle_revision: 0,
        capability_revision: format!("sha256:{id}"),
        created_at: 10,
        updated_at: 10,
    }
}

fn membership() -> SessionMembership {
    SessionMembership::new(
        SessionId::new("multi-session"),
        vec![
            instance("moderator-1", "orchestrator", SessionAgentRole::Moderator),
            instance("worker-1", "researcher", SessionAgentRole::Worker),
            instance(
                "coordinator-1",
                "orchestrator",
                SessionAgentRole::Coordinator {
                    swarm_id: SwarmId::new("swarm-1"),
                },
            ),
        ],
        SessionGovernance {
            session_id: SessionId::new("multi-session"),
            moderator_instance_id: AgentInstanceId::new("moderator-1"),
            governance_revision: "sha256:governance".into(),
            membership_revision: 0,
            lease_epoch: 3,
            fencing_token: 9,
            updated_at: 10,
        },
    )
    .unwrap()
}

fn topology(membership: &SessionMembership) -> SessionTopology {
    SessionTopology::new(
        SessionId::new("multi-session"),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        membership,
    )
    .unwrap()
}

fn dispatch_request(id: &str) -> DispatchMessageRequest {
    DispatchMessageRequest {
        message_id: CoordinationMessageId::new(id),
        session_id: SessionId::new("multi-session"),
        sender_instance_id: AgentInstanceId::new("worker-1"),
        recipient_instance_id: AgentInstanceId::new("coordinator-1"),
        task_id: None,
        kind: CoordinationMessageKind::Evidence,
        payload: "sha256:evidence".into(),
        max_hops: 4,
        expires_at: 100,
    }
}

#[tokio::test]
async fn defined_agent_joins_with_exact_configuration_atomically() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let request = DefineAgentRequest {
        instance_id: AgentInstanceId::new("defined-reviewer"),
        session_id: membership.session_id.clone(),
        sponsor_instance_id: membership.governance.moderator_instance_id.clone(),
        definition: AgentDefinitionKey {
            agent_id: AgentId::new("reviewer"),
            revision: 7,
        },
        role: SessionAgentRole::Reviewer,
        capability_revision: "sha256:reviewer".into(),
        effective_config: effective_config("reviewer"),
    };
    let DefineAgentOutcome::Created(created) =
        service.define_agent(request.clone(), 20).await.unwrap()
    else {
        panic!("valid defined Agent should be admitted");
    };
    let config = store
        .agent_instance_config(&created.session_id, &created.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(config.effective, request.effective_config);
    assert_eq!(config.config_revision, 0);
    assert_eq!(
        service.define_agent(request, 21).await.unwrap(),
        DefineAgentOutcome::Created(created)
    );
}

#[tokio::test]
async fn peer_and_review_relations_evolve_without_rewriting_ownership() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store, GovernancePolicy::default(), 30);
    let peer = RelateAgentsRequest {
        session_id: membership.session_id.clone(),
        requested_by: AgentInstanceId::new("worker-1"),
        source: AgentInstanceId::new("worker-1"),
        target: AgentInstanceId::new("coordinator-1"),
        kind: AgentRelationKind::Peer,
    };
    let RelateAgentsOutcome::Applied(topology) =
        service.relate_agents(peer.clone(), 20).await.unwrap()
    else {
        panic!("peer relation should be admitted");
    };
    assert_eq!(topology.topology_revision, 1);
    let RelateAgentsOutcome::Applied(unchanged) = service.relate_agents(peer, 21).await.unwrap()
    else {
        panic!("duplicate peer relation should be idempotent");
    };
    assert_eq!(unchanged.topology_revision, 1);
    let RelateAgentsOutcome::Applied(reviewed) = service
        .relate_agents(
            RelateAgentsRequest {
                session_id: membership.session_id,
                requested_by: AgentInstanceId::new("coordinator-1"),
                source: AgentInstanceId::new("coordinator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::Reviews,
            },
            22,
        )
        .await
        .unwrap()
    else {
        panic!("review relation should be admitted");
    };
    assert_eq!(reviewed.topology_revision, 2);
}

#[tokio::test]
async fn coordination_service_derives_route_from_durable_topology() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);

    let DispatchMessageOutcome::Enqueued(message) = service
        .dispatch_message(dispatch_request("governed-message"), 20)
        .await
        .unwrap()
    else {
        panic!("healthy dispatch must not require arbitration");
    };

    assert_eq!(
        message.route,
        ["worker-1", "moderator-1", "coordinator-1"].map(AgentInstanceId::new)
    );
    assert_eq!(
        store.message(&message.message_id).await.unwrap(),
        Some(message.clone())
    );
    assert_eq!(
        store.recoverable_message_recipients(20).await.unwrap(),
        vec![AgentInstanceId::new("coordinator-1")]
    );
    let repeated = service
        .dispatch_message(dispatch_request("governed-message"), 21)
        .await
        .unwrap();
    assert_eq!(repeated, DispatchMessageOutcome::Enqueued(message));
    let claim = service
        .claim_next_message(&AgentInstanceId::new("coordinator-1"), 22, 10)
        .await
        .unwrap()
        .unwrap();
    let delivered = service.mark_message_delivered(&claim, 23).await.unwrap();
    let acknowledged = service
        .acknowledge_message(&delivered, &AgentInstanceId::new("coordinator-1"), 24)
        .await
        .unwrap();
    assert_eq!(acknowledged.state, MessageDeliveryState::Acknowledged);
}

#[tokio::test]
async fn cancelling_running_work_fences_its_executor() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let task = service
        .create_task(
            CreateTaskRequest {
                task_id: TaskId::new("cancel-fences-task"),
                session_id: membership.session_id.clone(),
                parent_task_id: None,
                created_by: AgentInstanceId::new("worker-1"),
                assigned_to: AgentInstanceId::new("worker-1"),
                objective: "Stop without allowing a late commit".into(),
                token_budget: 1_000,
                max_handoffs: 0,
            },
            20,
        )
        .await
        .unwrap();
    let lease = service
        .claim_task(
            ClaimTaskRequest {
                task_id: task.task_id.clone(),
                session_id: membership.session_id.clone(),
                actor: AgentInstanceId::new("worker-1"),
                claim_owner_id: "turn-before-cancel".into(),
                lease_seconds: 30,
            },
            21,
        )
        .await
        .unwrap();
    let cancelled = service
        .cancel_task(
            crate::coordination::service::CancelTaskRequest {
                task_id: task.task_id,
                session_id: membership.session_id,
                actor: AgentInstanceId::new("worker-1"),
            },
            22,
        )
        .await
        .unwrap();
    assert_eq!(cancelled.state, CoordinationTaskState::Cancelled);
    assert!(
        service
            .finish_claimed_task(
                FinishClaimedTaskRequest {
                    lease,
                    next_state: CoordinationTaskState::Completed,
                    consumed_tokens: 100,
                },
                23,
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn interrupted_background_outbox_is_discoverable_from_durable_task_facts() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let task = service
        .create_task(
            CreateTaskRequest {
                task_id: TaskId::new("background-task:recovery-digest"),
                session_id: membership.session_id,
                parent_task_id: None,
                created_by: AgentInstanceId::new("worker-1"),
                assigned_to: AgentInstanceId::new("worker-1"),
                objective: "Recover the interrupted mailbox outbox".into(),
                token_budget: 1_000,
                max_handoffs: 0,
            },
            20,
        )
        .await
        .unwrap();
    assert_eq!(
        store.undispatched_background_tasks().await.unwrap(),
        vec![task]
    );
}

#[tokio::test]
async fn agents_drive_durable_tasks_with_runtime_owned_revision_fences() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let create = CreateTaskRequest {
        task_id: TaskId::new("agent-owned-task"),
        session_id: membership.session_id.clone(),
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: AgentInstanceId::new("worker-1"),
        objective: "Produce independently verifiable evidence".into(),
        token_budget: 1_000,
        max_handoffs: 2,
    };
    let task = service.create_task(create.clone(), 20).await.unwrap();
    assert_eq!(task.state, CoordinationTaskState::Ready);
    assert_eq!(service.create_task(create, 21).await.unwrap(), task);

    let lease = service
        .claim_task(
            ClaimTaskRequest {
                task_id: task.task_id.clone(),
                session_id: membership.session_id.clone(),
                actor: AgentInstanceId::new("worker-1"),
                claim_owner_id: "turn-agent-owned".into(),
                lease_seconds: 30,
            },
            22,
        )
        .await
        .unwrap();
    let running = store.task(&task.task_id).await.unwrap().unwrap();
    assert_eq!(running.revision, 1);
    let completed = service
        .finish_claimed_task(
            FinishClaimedTaskRequest {
                lease,
                next_state: CoordinationTaskState::Completed,
                consumed_tokens: 240,
            },
            23,
        )
        .await
        .unwrap();
    assert_eq!(completed.revision, 2);
    assert_eq!(completed.consumed_tokens, 240);
    assert_eq!(
        store.task(&completed.task_id).await.unwrap(),
        Some(completed)
    );
}

#[tokio::test]
async fn ordinary_agent_cannot_assign_work_outside_its_owned_branch() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store, GovernancePolicy::default(), 30);
    let error = service
        .create_task(
            CreateTaskRequest {
                task_id: TaskId::new("unauthorized-task"),
                session_id: membership.session_id,
                parent_task_id: None,
                created_by: AgentInstanceId::new("worker-1"),
                assigned_to: AgentInstanceId::new("coordinator-1"),
                objective: "Bypass governed ownership".into(),
                token_budget: 100,
                max_handoffs: 0,
            },
            20,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        crate::coordination::service::CoordinationServiceError::UnauthorizedActor
    ));
}

#[tokio::test]
async fn expired_task_lease_is_recovered_and_fences_the_old_executor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("task-leases.db");
    let store = Arc::new(SqliteSessionStore::open(&path).await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let task = service
        .create_task(
            CreateTaskRequest {
                task_id: TaskId::new("leased-task"),
                session_id: membership.session_id,
                parent_task_id: None,
                created_by: AgentInstanceId::new("moderator-1"),
                assigned_to: AgentInstanceId::new("worker-1"),
                objective: "Survive executor replacement".into(),
                token_budget: 1_000,
                max_handoffs: 1,
            },
            10,
        )
        .await
        .unwrap();
    let first = store
        .claim_task(
            &task.task_id,
            &AgentInstanceId::new("worker-1"),
            "turn-1",
            20,
            10,
        )
        .await
        .unwrap();
    assert_eq!(first.lease_epoch, 1);
    assert_eq!(first.task_revision, 1);
    assert_eq!(
        store
            .claim_task(
                &task.task_id,
                &AgentInstanceId::new("worker-1"),
                "turn-1",
                21,
                10,
            )
            .await
            .unwrap(),
        first
    );
    assert!(
        store
            .claim_task(
                &task.task_id,
                &AgentInstanceId::new("worker-1"),
                "turn-2",
                21,
                10,
            )
            .await
            .is_err()
    );

    let reopened = SqliteSessionStore::open(&path).await.unwrap();
    let recovered = reopened
        .claim_task(
            &task.task_id,
            &AgentInstanceId::new("worker-1"),
            "turn-2",
            30,
            10,
        )
        .await
        .unwrap();
    assert_eq!(recovered.lease_epoch, 2);
    assert_ne!(recovered.fencing_token, first.fencing_token);
    assert!(reopened.renew_task_lease(&first, 31, 10).await.is_err());
    assert!(
        reopened
            .finish_task_lease(&first, CoordinationTaskState::Completed, 100, 31)
            .await
            .is_err()
    );

    let renewed = reopened.renew_task_lease(&recovered, 31, 10).await.unwrap();
    assert_eq!(renewed.expires_at, 41);
    let completed = reopened
        .finish_task_lease(&renewed, CoordinationTaskState::Completed, 240, 32)
        .await
        .unwrap();
    assert_eq!(completed.state, CoordinationTaskState::Completed);
    assert_eq!(completed.revision, 2);
    assert_eq!(completed.consumed_tokens, 240);
}

#[tokio::test]
async fn fault_harness_reopens_and_fences_an_interrupted_task_executor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fault-task-lease.db");
    let store = Arc::new(SqliteSessionStore::open(&path).await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let task = service
        .create_task(
            CreateTaskRequest {
                task_id: TaskId::new("fault-task"),
                session_id: membership.session_id,
                parent_task_id: None,
                created_by: AgentInstanceId::new("moderator-1"),
                assigned_to: AgentInstanceId::new("worker-1"),
                objective: "Recover one interrupted execution".into(),
                token_budget: 1_000,
                max_handoffs: 1,
            },
            10,
        )
        .await
        .unwrap();
    let stale = store
        .claim_task(
            &task.task_id,
            &AgentInstanceId::new("worker-1"),
            "turn-before-crash",
            20,
            10,
        )
        .await
        .unwrap();
    let mut faults = FaultController::new(FaultInjectionSpec {
        point: FailurePoint::WorkflowTransitioned,
        occurrence: 1,
    })
    .unwrap();
    assert!(matches!(
        faults.checkpoint(FailurePoint::WorkflowTransitioned),
        FaultDecision::Interrupt(_)
    ));
    drop(service);
    drop(store);

    let recovered_store = SqliteSessionStore::open(path).await.unwrap();
    let recovered = recovered_store
        .claim_task(
            &task.task_id,
            &AgentInstanceId::new("worker-1"),
            "turn-after-crash",
            30,
            10,
        )
        .await
        .unwrap();
    assert_eq!(recovered.lease_epoch, stale.lease_epoch + 1);
    assert!(
        recovered_store
            .finish_task_lease(&stale, CoordinationTaskState::Completed, 50, 31)
            .await
            .is_err()
    );
    let completed = recovered_store
        .finish_task_lease(&recovered, CoordinationTaskState::Completed, 50, 31)
        .await
        .unwrap();
    assert_eq!(completed.state, CoordinationTaskState::Completed);
}

#[tokio::test]
async fn automatic_delivery_persists_one_turn_before_execution() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    service
        .dispatch_message(dispatch_request("auto-message"), 20)
        .await
        .unwrap();
    let claim = service
        .claim_next_message(&AgentInstanceId::new("coordinator-1"), 21, 10)
        .await
        .unwrap()
        .unwrap();

    let (delivered, receipt) = service
        .prepare_message_turn(&claim, "coordination:auto-message", 22)
        .await
        .unwrap();

    assert_eq!(delivered.state, MessageDeliveryState::Delivered);
    assert_eq!(receipt.turn_id, "coordination:auto-message");
    assert_eq!(
        store.message_turn(&delivered.message_id).await.unwrap(),
        Some(receipt.clone())
    );
    assert_eq!(
        store
            .recoverable_message_turns(&AgentInstanceId::new("coordinator-1"))
            .await
            .unwrap(),
        vec![(delivered.clone(), receipt.clone())]
    );
    assert_eq!(
        service
            .prepare_message_turn(&claim, &receipt.turn_id, 23)
            .await
            .unwrap(),
        (delivered, receipt)
    );
}

#[tokio::test]
async fn fault_harness_reopens_one_prepared_mailbox_turn_without_redelivery() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fault-mailbox.db");
    let store = Arc::new(SqliteSessionStore::open(&path).await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    service
        .dispatch_message(dispatch_request("fault-mailbox"), 20)
        .await
        .unwrap();
    let claim = service
        .claim_next_message(&AgentInstanceId::new("coordinator-1"), 21, 10)
        .await
        .unwrap()
        .unwrap();
    let expected = service
        .prepare_message_turn(&claim, "coordination:fault-mailbox", 22)
        .await
        .unwrap();
    let mut faults = FaultController::new(FaultInjectionSpec {
        point: FailurePoint::MailboxDelivered,
        occurrence: 1,
    })
    .unwrap();
    assert!(matches!(
        faults.checkpoint(FailurePoint::MailboxDelivered),
        FaultDecision::Interrupt(_)
    ));
    drop(service);
    drop(store);

    let recovered = SqliteSessionStore::open(path).await.unwrap();
    assert_eq!(
        recovered
            .recoverable_message_turns(&AgentInstanceId::new("coordinator-1"))
            .await
            .unwrap(),
        vec![expected]
    );
    assert!(
        recovered
            .claim_message(&AgentInstanceId::new("coordinator-1"), 23, 10)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn unresolved_mailbox_turn_is_idempotently_escalated_to_moderator() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    service
        .dispatch_message(dispatch_request("unresolved-message"), 20)
        .await
        .unwrap();
    let claim = service
        .claim_next_message(&AgentInstanceId::new("coordinator-1"), 21, 10)
        .await
        .unwrap()
        .unwrap();
    let (message, receipt) = service
        .prepare_message_turn(&claim, "coordination:unresolved", 22)
        .await
        .unwrap();

    let case = service
        .escalate_mailbox_turn(&message, &receipt, 23)
        .await
        .unwrap();

    assert!(case.has_hard_stop());
    assert_eq!(
        service
            .escalate_mailbox_turn(&message, &receipt, 24)
            .await
            .unwrap(),
        case
    );
    assert!(
        store
            .message(&CoordinationMessageId::new(format!(
                "arbitration:{}",
                case.case_id.0
            )))
            .await
            .unwrap()
            .is_some()
    );
    let decision = ModeratorDecision {
        case_id: case.case_id.clone(),
        decided_by: AgentInstanceId::new("moderator-1"),
        moderator_lease_epoch: 3,
        moderator_fencing_token: 9,
        verdict: ModeratorVerdict::SuspendAgents {
            agent_instance_ids: vec![AgentInstanceId::new("coordinator-1")],
        },
        rationale: "the interrupted effect requires explicit reconciliation".into(),
        evidence_refs: vec![format!("message:{}", message.message_id.0)],
        decided_at: 25,
    };
    let applied = service.decide_arbitration(&decision, 25).await.unwrap();
    assert_eq!(applied.state, ArbitrationState::Applied);
    assert_eq!(
        store
            .session_membership(&membership.session_id)
            .await
            .unwrap()
            .unwrap()
            .participants
            .into_iter()
            .find(|participant| participant.instance_id == AgentInstanceId::new("coordinator-1"))
            .unwrap()
            .state,
        AgentInstanceState::ManualReconciliation
    );
    assert_eq!(
        service.decide_arbitration(&decision, 26).await.unwrap(),
        applied
    );
}

#[tokio::test]
async fn coordination_service_persists_moderator_case_before_blocking_dispatch() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let policy = GovernancePolicy {
        max_agents: 2,
        ..GovernancePolicy::default()
    };
    let service = CoordinationService::new(store.clone(), policy, 30);

    let DispatchMessageOutcome::RequiresArbitration { case, assessment } = service
        .dispatch_message(dispatch_request("blocked-message"), 20)
        .await
        .unwrap()
    else {
        panic!("hard stop must require moderator arbitration");
    };

    assert!(assessment.has_hard_stop());
    assert!(case.case_id.0.starts_with("message:"));
    assert_eq!(case.case_id.0.len(), 72);
    assert_eq!(
        store.arbitration_case(&case.case_id).await.unwrap(),
        Some(case.clone())
    );
    assert!(
        store
            .message(&CoordinationMessageId::new("blocked-message"))
            .await
            .unwrap()
            .is_none()
    );
    let notification = store
        .message(&CoordinationMessageId::new(format!(
            "arbitration:{}",
            case.case_id.0
        )))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notification.recipient_instance_id.0, "moderator-1");
    assert_eq!(notification.kind, CoordinationMessageKind::Control);
    assert!(matches!(
        service
            .dispatch_message(dispatch_request("blocked-message"), 22)
            .await
            .unwrap(),
        DispatchMessageOutcome::RequiresArbitration { .. }
    ));
    let DispatchMessageOutcome::RequiresArbitration {
        case: renewed_case, ..
    } = service
        .dispatch_message(dispatch_request("blocked-message"), 51)
        .await
        .unwrap()
    else {
        panic!("an expired case must renew moderation for the stable intent");
    };
    assert_ne!(renewed_case.case_id, case.case_id);
    assert_eq!(
        store
            .arbitration_case(&case.case_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ArbitrationState::Expired
    );
}

#[tokio::test]
async fn moderator_conditions_authorize_the_exact_blocked_dispatch() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let task = CoordinationTask {
        task_id: TaskId::new("stagnant-task"),
        session_id: membership.session_id.clone(),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "produce distinct evidence".into(),
        state: CoordinationTaskState::Running,
        token_budget: 1_000,
        consumed_tokens: 100,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 10,
        updated_at: 10,
    };
    store.create_task(&task).await.unwrap();
    for sequence in 0..3 {
        store
            .record_progress(
                &task.session_id,
                &ProgressObservation {
                    observation_id: format!("stagnant-{sequence}"),
                    task_id: task.task_id.clone(),
                    agent_instance_id: AgentInstanceId::new("worker-1"),
                    task_revision: 0,
                    consumed_tokens: 110 + sequence,
                    evidence_digest: Some("sha256:same".into()),
                    observed_at: 11 + i64::try_from(sequence).unwrap(),
                },
            )
            .await
            .unwrap();
    }
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let request = dispatch_request("conditionally-authorized");
    let DispatchMessageOutcome::RequiresArbitration { case, assessment } =
        service.dispatch_message(request.clone(), 20).await.unwrap()
    else {
        panic!("stagnation must require moderator review");
    };
    assert!(!assessment.has_hard_stop());
    let decision = ModeratorDecision {
        case_id: case.case_id,
        decided_by: AgentInstanceId::new("moderator-1"),
        moderator_lease_epoch: 3,
        moderator_fencing_token: 9,
        verdict: ModeratorVerdict::ContinueWithConditions {
            conditions: vec!["attach a distinct evidence digest on the next iteration".into()],
        },
        rationale: "one bounded iteration can produce the missing evidence".into(),
        evidence_refs: vec!["progress:stagnant-2".into()],
        decided_at: 21,
    };
    service.decide_arbitration(&decision, 21).await.unwrap();

    let DispatchMessageOutcome::EnqueuedByModerator {
        message,
        decision: applied,
    } = service.dispatch_message(request, 22).await.unwrap()
    else {
        panic!("the exact moderated intent must proceed with its decision attached");
    };
    assert_eq!(message.message_id.0, "conditionally-authorized");
    assert_eq!(applied, decision);
}

#[tokio::test]
async fn coordination_service_recovers_handoff_at_arbitration_boundary() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    store
        .create_task(&CoordinationTask {
            task_id: TaskId::new("handoff-task"),
            session_id: SessionId::new("multi-session"),
            membership_revision: 0,
            parent_task_id: None,
            created_by: AgentInstanceId::new("moderator-1"),
            assigned_to: Some(AgentInstanceId::new("worker-1")),
            objective: "produce evidence".into(),
            state: CoordinationTaskState::Running,
            token_budget: 1_000,
            consumed_tokens: 100,
            max_handoffs: 2,
            handoff_count: 0,
            revision: 0,
            created_at: 10,
            updated_at: 10,
        })
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let request = ProposeHandoffRequest {
        handoff_id: HandoffId::new("governed-handoff"),
        session_id: SessionId::new("multi-session"),
        task_id: TaskId::new("handoff-task"),
        from_instance_id: AgentInstanceId::new("worker-1"),
        to_instance_id: AgentInstanceId::new("coordinator-1"),
        requested_by: AgentInstanceId::new("worker-1"),
        reason: "specialist context required".into(),
        expires_at: 100,
    };

    let handoff = service.propose_handoff(request.clone(), 20).await.unwrap();
    assert_eq!(handoff.state, HandoffState::AwaitingArbitration);
    assert_eq!(handoff.arbitrator_instance_id.0, "moderator-1");
    assert_eq!(service.propose_handoff(request, 21).await.unwrap(), handoff);
    let accepted = service
        .decide_handoff(
            &SessionId::new("multi-session"),
            &handoff.handoff_id,
            &AgentInstanceId::new("moderator-1"),
            true,
            22,
        )
        .await
        .unwrap();
    assert_eq!(accepted.state, HandoffState::Accepted);
    assert_eq!(
        service
            .decide_handoff(
                &SessionId::new("multi-session"),
                &handoff.handoff_id,
                &AgentInstanceId::new("moderator-1"),
                true,
                23,
            )
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        store
            .task(&TaskId::new("handoff-task"))
            .await
            .unwrap()
            .unwrap()
            .assigned_to,
        Some(AgentInstanceId::new("coordinator-1"))
    );
}

#[tokio::test]
async fn coordination_service_dead_letters_poison_message_after_bounded_retries() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let policy = GovernancePolicy {
        max_message_delivery_attempts: 1,
        ..GovernancePolicy::default()
    };
    let service = CoordinationService::new(store.clone(), policy, 30);
    service
        .dispatch_message(dispatch_request("poison-message"), 20)
        .await
        .unwrap();

    let first = service
        .claim_next_message(&AgentInstanceId::new("coordinator-1"), 21, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.message.delivery_attempts, 1);
    assert!(
        service
            .claim_next_message(&AgentInstanceId::new("coordinator-1"), 22, 1)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .message(&CoordinationMessageId::new("poison-message"))
            .await
            .unwrap()
            .unwrap()
            .state,
        MessageDeliveryState::DeadLetter
    );
}

#[tokio::test]
async fn governance_wait_and_progress_facts_are_revision_fenced_and_idempotent() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let task = CoordinationTask {
        task_id: TaskId::new("observed-task"),
        session_id: SessionId::new("multi-session"),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "observe governed progress".into(),
        state: CoordinationTaskState::Running,
        token_budget: 1_000,
        consumed_tokens: 100,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 10,
        updated_at: 10,
    };
    store.create_task(&task).await.unwrap();
    let wait = WaitDependency {
        task_id: task.task_id.clone(),
        waiter: AgentInstanceId::new("worker-1"),
        awaited: AgentInstanceId::new("coordinator-1"),
    };
    store
        .record_wait(&task.session_id, &wait, 0, 0, 20)
        .await
        .unwrap();
    store
        .record_wait(&task.session_id, &wait, 0, 0, 21)
        .await
        .unwrap();
    assert!(
        store
            .record_wait(&task.session_id, &wait, 0, 1, 22)
            .await
            .is_err()
    );

    let progress = ProgressObservation {
        observation_id: "progress-1".into(),
        task_id: task.task_id.clone(),
        agent_instance_id: AgentInstanceId::new("worker-1"),
        task_revision: 0,
        consumed_tokens: 150,
        evidence_digest: Some("sha256:new-evidence".into()),
        observed_at: 20,
    };
    store
        .record_progress(&task.session_id, &progress)
        .await
        .unwrap();
    store
        .record_progress(&task.session_id, &progress)
        .await
        .unwrap();
    let mut conflicting = progress;
    conflicting.consumed_tokens = 151;
    assert!(
        store
            .record_progress(&task.session_id, &conflicting)
            .await
            .is_err()
    );
    let observations = store
        .governance_observations(&task.session_id, 4)
        .await
        .unwrap();
    assert_eq!(observations.waits.len(), 1);
    assert_eq!(observations.waits[0], wait);
    assert_eq!(observations.progress.len(), 1);
    assert_eq!(observations.progress[0].observation_id, "progress-1");
    assert!(observations.handoffs.is_empty());
    store.clear_wait(&task.session_id, &wait).await.unwrap();
    store.clear_wait(&task.session_id, &wait).await.unwrap();
    assert!(
        store
            .governance_observations(&task.session_id, 4)
            .await
            .unwrap()
            .waits
            .is_empty()
    );
}

#[tokio::test]
async fn durable_wait_cycle_blocks_dispatch_and_escalates_to_moderator() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let mut task = CoordinationTask {
        task_id: TaskId::new("cycle-left"),
        session_id: SessionId::new("multi-session"),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "left side".into(),
        state: CoordinationTaskState::Running,
        token_budget: 1_000,
        consumed_tokens: 10,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 10,
        updated_at: 10,
    };
    store.create_task(&task).await.unwrap();
    task.task_id = TaskId::new("cycle-right");
    task.assigned_to = Some(AgentInstanceId::new("coordinator-1"));
    task.objective = "right side".into();
    store.create_task(&task).await.unwrap();
    let service = CoordinationService::new(store, GovernancePolicy::default(), 30);
    service
        .report_wait(
            &ReportWaitRequest {
                session_id: SessionId::new("multi-session"),
                task_id: TaskId::new("cycle-left"),
                waiter: AgentInstanceId::new("worker-1"),
                awaited: AgentInstanceId::new("coordinator-1"),
            },
            20,
        )
        .await
        .unwrap();
    service
        .report_wait(
            &ReportWaitRequest {
                session_id: SessionId::new("multi-session"),
                task_id: TaskId::new("cycle-right"),
                waiter: AgentInstanceId::new("coordinator-1"),
                awaited: AgentInstanceId::new("worker-1"),
            },
            20,
        )
        .await
        .unwrap();

    let DispatchMessageOutcome::RequiresArbitration { assessment, .. } = service
        .dispatch_message(dispatch_request("cycle-blocked-message"), 21)
        .await
        .unwrap()
    else {
        panic!("durable wait cycle must block automatic dispatch");
    };
    assert!(assessment.findings.iter().any(|finding| matches!(
        finding,
        GovernanceFinding::WaitCycle { agents } if agents.len() == 2
    )));
}

#[tokio::test]
async fn governed_fork_is_idempotent_and_reconciles_task_membership_revision() {
    let store = Arc::new(SqliteSessionStore::open_in_memory().await.unwrap());
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    bind_test_config(&store, "worker-1", "researcher").await;
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    store
        .create_task(&CoordinationTask {
            task_id: TaskId::new("fork-existing-task"),
            session_id: SessionId::new("multi-session"),
            membership_revision: 0,
            parent_task_id: None,
            created_by: AgentInstanceId::new("moderator-1"),
            assigned_to: Some(AgentInstanceId::new("worker-1")),
            objective: "survive participant append".into(),
            state: CoordinationTaskState::Running,
            token_budget: 1_000,
            consumed_tokens: 10,
            max_handoffs: 2,
            handoff_count: 0,
            revision: 0,
            created_at: 10,
            updated_at: 10,
        })
        .await
        .unwrap();
    let view = AgentWorkspaceView {
        view_id: WorkspaceViewId::new("worker-view"),
        session_id: membership.session_id.clone(),
        agent_instance_id: AgentInstanceId::new("worker-1"),
        membership_revision: 0,
        access: WorkspaceAccess::ReadOnly,
        isolation: WorkspaceIsolation::Shared,
        source_workspace: PathBuf::from("/tmp/project"),
        effective_workspace: PathBuf::from("/tmp/project"),
        target_id: None,
        branch: None,
        base_revision: None,
        state: WorkspaceViewState::Provisioning,
        lease_epoch: 3,
        fencing_token: 9,
        revision: 0,
        created_at: 10,
        updated_at: 10,
    };
    store
        .create_workspace_view(&view, &membership)
        .await
        .unwrap();
    let service = CoordinationService::new(store.clone(), GovernancePolicy::default(), 30);
    let request = ForkAgentRequest {
        instance_id: AgentInstanceId::new("fork-child"),
        session_id: SessionId::new("multi-session"),
        parent_instance_id: AgentInstanceId::new("worker-1"),
        branch_id: "branch-fork-child".into(),
    };

    let ForkAgentOutcome::Created(child) = service.fork_agent(request.clone(), 20).await.unwrap()
    else {
        panic!("bounded fork should not require arbitration");
    };
    assert_eq!(child.state, AgentInstanceState::Created);
    let parent_config = store
        .agent_instance_config(&child.session_id, &AgentInstanceId::new("worker-1"))
        .await
        .unwrap()
        .unwrap();
    let child_config = store
        .agent_instance_config(&child.session_id, &child.instance_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child_config.effective, parent_config.effective);
    assert_eq!(child_config.config_revision, parent_config.config_revision);
    let ready = service.mark_agent_ready(&child, 21).await.unwrap();
    assert_eq!(ready.state, AgentInstanceState::Ready);
    assert_eq!(
        service.fork_agent(request, 22).await.unwrap(),
        ForkAgentOutcome::Created(ready.clone())
    );
    let restored = store
        .session_membership(&SessionId::new("multi-session"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.governance.membership_revision, 1);
    assert_eq!(restored.participants.last(), Some(&ready));
    assert_eq!(
        store
            .topology(&SessionId::new("multi-session"))
            .await
            .unwrap()
            .unwrap()
            .topology_revision,
        1
    );
    assert_eq!(
        store
            .task(&TaskId::new("fork-existing-task"))
            .await
            .unwrap()
            .unwrap()
            .membership_revision,
        1
    );
    assert_eq!(
        store
            .workspace_view(&view.view_id)
            .await
            .unwrap()
            .unwrap()
            .membership_revision,
        1
    );
}

#[tokio::test]
async fn fork_history_cursor_is_runtime_derived_and_stable() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    bind_test_config(&store, "worker-1", "researcher").await;
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let parent_context =
        sylvander_api::SessionContext::new("user-1", "researcher", "multi-session")
            .with_agent_instance("worker-1");
    store
        .append_message(
            &parent_context,
            &SessionId::new("multi-session"),
            MessageRole::User,
            serde_json::json!("first"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    store
        .append_message(
            &parent_context,
            &SessionId::new("multi-session"),
            MessageRole::Assistant,
            serde_json::json!("second"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let service =
        CoordinationService::new(Arc::new(store.clone()), GovernancePolicy::default(), 30);
    let request = ForkAgentRequest {
        instance_id: AgentInstanceId::new("history-child"),
        session_id: SessionId::new("multi-session"),
        parent_instance_id: AgentInstanceId::new("worker-1"),
        branch_id: "history-child".into(),
    };
    let ForkAgentOutcome::Created(child) = service.fork_agent(request.clone(), 20).await.unwrap()
    else {
        panic!("history fork should be admitted");
    };
    assert!(matches!(
        &child.history_view,
        HistoryView::ForkSnapshot {
            base_sequence: 2,
            ..
        }
    ));
    store
        .append_message(
            &parent_context,
            &SessionId::new("multi-session"),
            MessageRole::Assistant,
            serde_json::json!("after snapshot"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        service.fork_agent(request, 21).await.unwrap(),
        ForkAgentOutcome::Created(child.clone())
    );
    assert_eq!(
        store
            .materialize_agent_fork_history(
                &child.session_id,
                &AgentInstanceId::new("worker-1"),
                &child.instance_id,
                2,
                22,
            )
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        store
            .materialize_agent_fork_history(
                &child.session_id,
                &AgentInstanceId::new("worker-1"),
                &child.instance_id,
                2,
                23,
            )
            .await
            .unwrap(),
        2
    );
    let child_context = sylvander_api::SessionContext::new("user-1", "researcher", "multi-session")
        .with_agent_instance("history-child");
    let history = store
        .read_history(
            &child_context,
            &SessionId::new("multi-session"),
            false,
            None,
        )
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].content, serde_json::json!("first"));
    assert_eq!(history[1].content, serde_json::json!("second"));
}

#[tokio::test]
async fn empty_fork_cursor_excludes_messages_appended_after_snapshot() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    bind_test_config(&store, "worker-1", "researcher").await;
    store
        .save_topology(&topology(&membership), &membership, None)
        .await
        .unwrap();
    let service =
        CoordinationService::new(Arc::new(store.clone()), GovernancePolicy::default(), 30);
    let ForkAgentOutcome::Created(child) = service
        .fork_agent(
            ForkAgentRequest {
                instance_id: AgentInstanceId::new("empty-history-child"),
                session_id: SessionId::new("multi-session"),
                parent_instance_id: AgentInstanceId::new("worker-1"),
                branch_id: "empty-history-child".into(),
            },
            20,
        )
        .await
        .unwrap()
    else {
        panic!("empty history fork should be admitted");
    };
    assert!(matches!(
        &child.history_view,
        HistoryView::ForkSnapshot {
            base_sequence: 0,
            ..
        }
    ));
    let parent_context =
        sylvander_api::SessionContext::new("user-1", "researcher", "multi-session")
            .with_agent_instance("worker-1");
    store
        .append_message(
            &parent_context,
            &child.session_id,
            MessageRole::User,
            serde_json::json!("after empty snapshot"),
            None,
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        store
            .materialize_agent_fork_history(
                &child.session_id,
                &AgentInstanceId::new("worker-1"),
                &child.instance_id,
                0,
                21,
            )
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn multi_agent_membership_survives_file_restart() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("sessions.db");
    {
        let store = SqliteSessionStore::open(&path).await.unwrap();
        store.save(&stored_session()).await.unwrap();
        store
            .save_session_membership(&membership(), None)
            .await
            .unwrap();
    }

    let reopened = SqliteSessionStore::open(path).await.unwrap();
    let restored = reopened
        .session_membership(&SessionId::new("multi-session"))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(restored, membership());
    assert_eq!(restored.participants.len(), 3);
    assert_eq!(restored.moderator().instance_id.0, "moderator-1");
}

#[tokio::test]
async fn membership_requires_an_existing_active_session() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let error = store
        .save_session_membership(&membership(), None)
        .await
        .unwrap_err();

    assert!(matches!(error, SessionStoreError::NotFound(_)));
}

#[tokio::test]
async fn replacing_membership_removes_departed_instances_atomically() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    store
        .save_session_membership(&membership(), None)
        .await
        .unwrap();
    let reduced = SessionMembership::new(
        SessionId::new("multi-session"),
        vec![instance(
            "moderator-2",
            "orchestrator",
            SessionAgentRole::Moderator,
        )],
        SessionGovernance {
            moderator_instance_id: AgentInstanceId::new("moderator-2"),
            membership_revision: 1,
            fencing_token: 10,
            ..membership().governance
        },
    )
    .unwrap();

    store
        .save_session_membership(&reduced, Some(0))
        .await
        .unwrap();
    let restored = store
        .session_membership(&SessionId::new("multi-session"))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(restored, reduced);
    assert_eq!(restored.participants.len(), 1);
}

#[tokio::test]
async fn stale_membership_writer_is_rejected_without_overwriting_snapshot() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    store
        .save_session_membership(&membership(), None)
        .await
        .unwrap();

    let error = store
        .save_session_membership(&membership(), None)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        SessionStoreError::MembershipConflict {
            expected: None,
            actual: Some(0)
        }
    ));
    assert_eq!(
        store
            .session_membership(&SessionId::new("multi-session"))
            .await
            .unwrap()
            .unwrap(),
        membership()
    );
}

#[tokio::test]
async fn topology_is_durable_and_fenced_by_its_own_revision() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let topology = SessionTopology::new(
        SessionId::new("multi-session"),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        &membership,
    )
    .unwrap();

    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    assert_eq!(
        store
            .topology(&SessionId::new("multi-session"))
            .await
            .unwrap(),
        Some(topology.clone())
    );
    assert!(matches!(
        store
            .save_topology(&topology, &membership, None)
            .await
            .unwrap_err(),
        SessionStoreError::TopologyConflict {
            expected: None,
            actual: Some(0)
        }
    ));
}

#[tokio::test]
async fn participant_append_updates_membership_and_topology_in_one_transaction() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    bind_test_config(&store, "worker-1", "researcher").await;
    let topology = topology(&membership);
    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    let mut child = instance("fork-1", "researcher", SessionAgentRole::Worker);
    child.origin = AgentInstanceOrigin::Forked {
        parent_instance_id: AgentInstanceId::new("worker-1"),
        fork_sequence: 1,
    };
    child.history_view = HistoryView::ForkSnapshot {
        base_sequence: 0,
        branch_id: "fork-1".into(),
    };
    child.approval_route = ApprovalRoute::Parent {
        instance_id: AgentInstanceId::new("worker-1"),
    };
    child.state = AgentInstanceState::Created;
    let mut participants = membership.participants.clone();
    participants.push(child.clone());
    let next_membership = SessionMembership::new(
        membership.session_id.clone(),
        participants,
        SessionGovernance {
            membership_revision: 1,
            updated_at: 20,
            ..membership.governance.clone()
        },
    )
    .unwrap();
    let mut relations = topology.relations.clone();
    relations.push(AgentRelation {
        source: AgentInstanceId::new("worker-1"),
        target: child.instance_id.clone(),
        kind: AgentRelationKind::ParentOf,
        created_at: 20,
    });
    let next_topology = SessionTopology::new(
        membership.session_id.clone(),
        1,
        1,
        relations,
        20,
        &next_membership,
    )
    .unwrap();

    let committed = store
        .add_session_participant(
            &child,
            AgentInstanceConfigSeed::InheritFrom(AgentInstanceId::new("worker-1")),
            &next_membership,
            &next_topology,
            0,
            0,
        )
        .await
        .unwrap();
    assert_eq!(committed, child);
    assert_eq!(
        store
            .session_membership(&membership.session_id)
            .await
            .unwrap(),
        Some(next_membership.clone())
    );
    assert_eq!(
        store.topology(&membership.session_id).await.unwrap(),
        Some(next_topology.clone())
    );
    assert!(matches!(
        store
            .add_session_participant(
                &child,
                AgentInstanceConfigSeed::InheritFrom(AgentInstanceId::new("worker-1")),
                &next_membership,
                &next_topology,
                0,
                0,
            )
            .await
            .unwrap_err(),
        SessionStoreError::MembershipConflict {
            expected: Some(0),
            actual: Some(1)
        }
    ));
    let ready = store
        .transition_agent_instance(
            &membership.session_id,
            &child.instance_id,
            0,
            AgentInstanceState::Ready,
            21,
        )
        .await
        .unwrap();
    assert_eq!(ready.state, AgentInstanceState::Ready);
    assert_eq!(ready.lifecycle_revision, 1);
    assert!(
        store
            .transition_agent_instance(
                &membership.session_id,
                &child.instance_id,
                0,
                AgentInstanceState::Running,
                22,
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn task_creation_is_durable_and_duplicate_safe() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    store
        .save_session_membership(&membership(), None)
        .await
        .unwrap();
    let task = CoordinationTask {
        task_id: TaskId::new("task-1"),
        session_id: SessionId::new("multi-session"),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "inspect recovery invariants".into(),
        state: CoordinationTaskState::Proposed,
        token_budget: 2_000,
        consumed_tokens: 0,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 12,
        updated_at: 12,
    };

    store.create_task(&task).await.unwrap();
    assert_eq!(store.task(&task.task_id).await.unwrap(), Some(task.clone()));
    assert!(matches!(
        store.create_task(&task).await.unwrap_err(),
        SessionStoreError::TaskConflict {
            expected: None,
            actual: Some(0),
            ..
        }
    ));

    let mut ready = task.clone();
    ready.state = CoordinationTaskState::Ready;
    ready.consumed_tokens = 25;
    ready.revision = 1;
    ready.updated_at = 13;
    store.update_task(&ready, 0).await.unwrap();
    assert_eq!(
        store.task(&task.task_id).await.unwrap(),
        Some(ready.clone())
    );

    let mut stale = ready.clone();
    stale.revision = 1;
    assert!(matches!(
        store.update_task(&stale, 0).await.unwrap_err(),
        SessionStoreError::TaskConflict {
            expected: Some(0),
            actual: Some(1),
            ..
        }
    ));

    let mut illicit_assignment = ready;
    illicit_assignment.assigned_to = Some(AgentInstanceId::new("coordinator-1"));
    illicit_assignment.state = CoordinationTaskState::Running;
    illicit_assignment.revision = 2;
    illicit_assignment.updated_at = 14;
    assert!(matches!(
        store.update_task(&illicit_assignment, 1).await.unwrap_err(),
        SessionStoreError::Invalid(_)
    ));
}

#[tokio::test]
async fn task_dependencies_are_durable_and_cycles_roll_back_atomically() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let first = CoordinationTask {
        task_id: TaskId::new("task-first"),
        session_id: membership.session_id.clone(),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "collect evidence".into(),
        state: CoordinationTaskState::Ready,
        token_budget: 1_000,
        consumed_tokens: 0,
        max_handoffs: 1,
        handoff_count: 0,
        revision: 0,
        created_at: 12,
        updated_at: 12,
    };
    let mut second = first.clone();
    second.task_id = TaskId::new("task-second");
    second.assigned_to = Some(AgentInstanceId::new("coordinator-1"));
    second.objective = "synthesize evidence".into();
    store.create_task(&first).await.unwrap();
    store.create_task(&second).await.unwrap();

    let forward = TaskDependency {
        prerequisite: first.task_id.clone(),
        dependent: second.task_id.clone(),
    };
    store
        .add_task_dependency(&forward, &membership, 13)
        .await
        .unwrap();
    let graph = store
        .task_graph(&membership.session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(graph.dependencies, std::slice::from_ref(&forward));

    let reverse = TaskDependency {
        prerequisite: second.task_id,
        dependent: first.task_id,
    };
    assert!(matches!(
        store
            .add_task_dependency(&reverse, &membership, 14)
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(reason) if reason.contains("cycle")
    ));
    assert_eq!(
        store
            .task_graph(&membership.session_id)
            .await
            .unwrap()
            .unwrap()
            .dependencies,
        [forward]
    );
}

#[tokio::test]
async fn arbitration_case_is_durable_and_fenced_to_exact_governance_facts() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let topology = SessionTopology::new(
        membership.session_id.clone(),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        &membership,
    )
    .unwrap();
    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    let task = CoordinationTask {
        task_id: TaskId::new("task-1"),
        session_id: membership.session_id.clone(),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "produce fresh evidence".into(),
        state: CoordinationTaskState::Running,
        token_budget: 1_000,
        consumed_tokens: 200,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 11,
        updated_at: 11,
    };
    store.create_task(&task).await.unwrap();
    let case = ArbitrationCase {
        case_id: GovernanceCaseId::new("case-1"),
        session_id: membership.session_id.clone(),
        moderator_instance_id: AgentInstanceId::new("moderator-1"),
        membership_revision: 0,
        topology_revision: 0,
        moderator_lease_epoch: 3,
        moderator_fencing_token: 9,
        findings: vec![GovernanceFinding::StagnantProgress {
            task_id: TaskId::new("task-1"),
            observations: 3,
        }],
        state: ArbitrationState::Open,
        revision: 0,
        expires_at: 100,
        created_at: 12,
        updated_at: 12,
    };

    store
        .create_arbitration_case(&case, &membership, &topology, 50)
        .await
        .unwrap();
    assert_eq!(
        store.arbitration_case(&case.case_id).await.unwrap(),
        Some(case.clone())
    );
    assert!(matches!(
        store
            .create_arbitration_case(&case, &membership, &topology, 50)
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(reason) if reason.contains("already exists")
    ));

    let decision = ModeratorDecision {
        case_id: case.case_id.clone(),
        decided_by: AgentInstanceId::new("moderator-1"),
        moderator_lease_epoch: 3,
        moderator_fencing_token: 9,
        verdict: ModeratorVerdict::Replan {
            task_ids: vec![task.task_id],
        },
        rationale: "the current plan consumed tokens without new evidence".into(),
        evidence_refs: vec!["progress:3".into()],
        decided_at: 60,
    };
    let graph = store
        .task_graph(&membership.session_id)
        .await
        .unwrap()
        .unwrap();
    let decided = store
        .decide_arbitration(&decision, &membership, &graph, 0, 60)
        .await
        .unwrap();
    assert_eq!(decided.state, ArbitrationState::Applied);
    assert_eq!(decided.revision, 1);
    let replanned = store.task(&TaskId::new("task-1")).await.unwrap().unwrap();
    assert_eq!(replanned.state, CoordinationTaskState::Blocked);
    assert_eq!(replanned.revision, 1);
    assert_eq!(
        store.arbitration_decision(&case.case_id).await.unwrap(),
        Some(decision.clone())
    );
    assert!(matches!(
        store
            .decide_arbitration(&decision, &membership, &graph, 0, 61)
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(_)
    ));

    let mut stale = case;
    stale.case_id = GovernanceCaseId::new("case-stale");
    stale.moderator_fencing_token = 8;
    assert!(matches!(
        store
            .create_arbitration_case(&stale, &membership, &topology, 50)
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(reason) if reason.contains("governance")
    ));
}

#[tokio::test]
async fn handoff_proposal_is_validated_persisted_and_deduplicated() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let topology = SessionTopology::new(
        SessionId::new("multi-session"),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        &membership,
    )
    .unwrap();
    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    let task = CoordinationTask {
        task_id: TaskId::new("task-handoff"),
        session_id: SessionId::new("multi-session"),
        membership_revision: 0,
        parent_task_id: None,
        created_by: AgentInstanceId::new("moderator-1"),
        assigned_to: Some(AgentInstanceId::new("worker-1")),
        objective: "transfer governed work".into(),
        state: CoordinationTaskState::Running,
        token_budget: 2_000,
        consumed_tokens: 100,
        max_handoffs: 2,
        handoff_count: 0,
        revision: 0,
        created_at: 12,
        updated_at: 12,
    };
    store.create_task(&task).await.unwrap();
    let handoff = TaskHandoff {
        handoff_id: HandoffId::new("handoff-1"),
        session_id: task.session_id.clone(),
        task_id: task.task_id.clone(),
        from_instance_id: AgentInstanceId::new("worker-1"),
        to_instance_id: AgentInstanceId::new("coordinator-1"),
        requested_by: AgentInstanceId::new("worker-1"),
        arbitrator_instance_id: AgentInstanceId::new("moderator-1"),
        task_revision: 0,
        topology_revision: 0,
        reason: "coordinator owns the next stage".into(),
        state: HandoffState::Proposed,
        revision: 0,
        expires_at: 100,
        created_at: 13,
        updated_at: 13,
    };

    store
        .create_handoff(&handoff, &membership, &topology, 50)
        .await
        .unwrap();
    assert_eq!(
        store.handoff(&handoff.handoff_id).await.unwrap(),
        Some(handoff.clone())
    );
    assert!(matches!(
        store
            .create_handoff(&handoff, &membership, &topology, 50)
            .await
            .unwrap_err(),
        SessionStoreError::HandoffConflict {
            expected: None,
            actual: Some(0),
            ..
        }
    ));

    let awaiting = store
        .transition_handoff(
            &handoff.handoff_id,
            &AgentInstanceId::new("worker-1"),
            HandoffState::AwaitingArbitration,
            0,
            60,
        )
        .await
        .unwrap();
    assert_eq!(awaiting.revision, 1);
    let accepted = store
        .transition_handoff(
            &handoff.handoff_id,
            &AgentInstanceId::new("moderator-1"),
            HandoffState::Accepted,
            1,
            70,
        )
        .await
        .unwrap();
    assert_eq!(accepted.state, HandoffState::Accepted);
    let reassigned = store.task(&task.task_id).await.unwrap().unwrap();
    assert_eq!(
        reassigned.assigned_to,
        Some(AgentInstanceId::new("coordinator-1"))
    );
    assert_eq!(reassigned.handoff_count, 1);
    assert_eq!(reassigned.revision, 1);
    assert!(matches!(
        store
            .transition_handoff(
                &handoff.handoff_id,
                &AgentInstanceId::new("moderator-1"),
                HandoffState::Accepted,
                1,
                71,
            )
            .await
            .unwrap_err(),
        SessionStoreError::HandoffConflict {
            expected: Some(1),
            actual: Some(2),
            ..
        }
    ));
}

#[tokio::test]
async fn mailbox_message_is_durable_and_deduplicated_before_delivery() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.save(&stored_session()).await.unwrap();
    let membership = membership();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    let topology = SessionTopology::new(
        SessionId::new("multi-session"),
        0,
        0,
        vec![
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("worker-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
            AgentRelation {
                source: AgentInstanceId::new("moderator-1"),
                target: AgentInstanceId::new("coordinator-1"),
                kind: AgentRelationKind::ParentOf,
                created_at: 11,
            },
        ],
        11,
        &membership,
    )
    .unwrap();
    store
        .save_topology(&topology, &membership, None)
        .await
        .unwrap();
    let message = CoordinationMessage {
        message_id: CoordinationMessageId::new("message-1"),
        session_id: SessionId::new("multi-session"),
        sender_instance_id: AgentInstanceId::new("worker-1"),
        recipient_instance_id: AgentInstanceId::new("coordinator-1"),
        task_id: None,
        kind: CoordinationMessageKind::Evidence,
        payload: "content-free result digest".into(),
        topology_revision: 0,
        route: topology
            .route_between(
                &AgentInstanceId::new("worker-1"),
                &AgentInstanceId::new("coordinator-1"),
            )
            .unwrap(),
        max_hops: 4,
        state: MessageDeliveryState::Pending,
        delivery_attempts: 0,
        revision: 0,
        expires_at: 100,
        created_at: 12,
        updated_at: 12,
    };

    store
        .enqueue_message(&message, &membership, &topology, 50)
        .await
        .unwrap();
    assert_eq!(
        store.message(&message.message_id).await.unwrap(),
        Some(message.clone())
    );
    assert!(matches!(
        store
            .enqueue_message(&message, &membership, &topology, 50)
            .await
            .unwrap_err(),
        SessionStoreError::MessageConflict {
            expected: None,
            actual: Some(0),
            ..
        }
    ));

    let first_claim = store
        .claim_message(&AgentInstanceId::new("coordinator-1"), 50, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first_claim.lease_epoch, 1);
    assert!(
        store
            .claim_message(&AgentInstanceId::new("coordinator-1"), 55, 10)
            .await
            .unwrap()
            .is_none()
    );
    let recovered_claim = store
        .claim_message(&AgentInstanceId::new("coordinator-1"), 60, 10)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered_claim.lease_epoch, 2);
    assert!(matches!(
        store
            .finish_message_claim(
                &message.message_id,
                &AgentInstanceId::new("coordinator-1"),
                first_claim.lease_epoch,
                MessageDeliveryState::Delivered,
                61,
            )
            .await
            .unwrap_err(),
        SessionStoreError::MessageConflict { .. }
    ));
    let delivered = store
        .finish_message_claim(
            &message.message_id,
            &AgentInstanceId::new("coordinator-1"),
            recovered_claim.lease_epoch,
            MessageDeliveryState::Delivered,
            61,
        )
        .await
        .unwrap();
    assert_eq!(delivered.state, MessageDeliveryState::Delivered);
    assert_eq!(delivered.delivery_attempts, 2);
    assert_eq!(delivered.revision, 3);
    let acknowledged = store
        .acknowledge_message(
            &message.message_id,
            &AgentInstanceId::new("coordinator-1"),
            delivered.revision,
            62,
        )
        .await
        .unwrap();
    assert_eq!(acknowledged.state, MessageDeliveryState::Acknowledged);
    assert_eq!(acknowledged.revision, 4);
    assert!(matches!(
        store
            .acknowledge_message(
                &message.message_id,
                &AgentInstanceId::new("worker-1"),
                delivered.revision,
                63,
            )
            .await
            .unwrap_err(),
        SessionStoreError::MessageConflict {
            expected: Some(3),
            actual: Some(4),
            ..
        }
    ));
}
