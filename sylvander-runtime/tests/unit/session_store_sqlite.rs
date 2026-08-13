use super::*;
use base64::Engine as _;
use std::path::PathBuf;
use std::sync::Arc;
use sylvander_agent::workspace_journal::WorkspaceMutationJournal;
use sylvander_llm_core::{
    AudioContent, AudioFormat, ContentBlock, ModelCapabilities, ModelEventStream, ModelInfo,
    ModelProvider, ModelRef, ModelRequest, ModelResponse, ModelStreamEvent, ProviderFuture,
    StopReason, TokenUsage,
};

use crate::agent::cognition::CognitiveRole;
use crate::agent::instance::{
    AgentDefinitionKey, AgentInstance, AgentInstanceOrigin, AgentInstanceState, ApprovalRoute,
    HistoryView, SessionAgentRole,
};
use crate::agent::perception::{
    PerceptionArtifactKind, PerceptionArtifactStore, PerceptionModality,
};
use crate::agent::perception_execution::{
    PerceptionExecutionRequest, execute_perception, recover_perception_receipt,
};
use crate::evidence::{EvidenceEncryption, EvidenceGovernance, EvidenceStore};
use crate::session::membership::{SessionGovernance, SessionMembership};
use crate::storage::agent_instance::{AgentInstanceConfig, AgentInstanceStore};
use crate::storage::artifact::{ArtifactTurnBinding, RuntimeArtifactService};
use crate::storage::session::{
    ModelRecoveryClassification, ModelRecoveryDecision, ModelRecoveryReason,
};
use crate::storage::workspace_journal::WorkspaceJournal;

/// Default session context used by every test. Identity is the
/// stable "user-1" from `test_meta` so ownership assertions share one
/// authenticated subject.
fn ctx() -> sylvander_api::SessionContext {
    sylvander_api::SessionContext::new("user-1", "agent-1", "sess-1")
}

fn turn_instance() -> AgentInstanceId {
    AgentInstanceId::new("test-instance")
}

fn turn_ctx() -> sylvander_api::SessionContext {
    ctx().with_agent_instance(turn_instance())
}

async fn persist_turn_member(
    store: &SqliteSessionStore,
    session: &StoredSession,
    effective: &sylvander_api::SessionEffectiveConfig,
) {
    let instance_id = turn_instance();
    let membership = SessionMembership::new(
        session.id.clone(),
        vec![AgentInstance {
            instance_id: instance_id.clone(),
            session_id: session.id.clone(),
            definition: AgentDefinitionKey {
                agent_id: effective.agent_id.clone(),
                revision: effective.agent_revision,
            },
            origin: AgentInstanceOrigin::Defined,
            role: SessionAgentRole::Moderator,
            history_view: HistoryView::SharedLane { cursor: 0 },
            approval_route: ApprovalRoute::User,
            state: AgentInstanceState::Ready,
            lifecycle_revision: 0,
            capability_revision: "test-capability".into(),
            created_at: session.created_at,
            updated_at: session.updated_at,
        }],
        SessionGovernance {
            session_id: session.id.clone(),
            moderator_instance_id: instance_id,
            governance_revision: "test-governance".into(),
            membership_revision: 0,
            lease_epoch: 1,
            fencing_token: 1,
            updated_at: session.updated_at,
        },
    )
    .unwrap();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
}

fn test_meta() -> SessionMetadata {
    SessionMetadata {
        workspace: PathBuf::from("/tmp"),
        name: "test".into(),
        user_id: "user-1".into(),
    }
}

fn make_session(id: &str, lifetime: SessionLifetime) -> StoredSession {
    StoredSession::new(
        SessionId::new(id),
        format!("session-{id}"),
        lifetime,
        test_meta(),
        vec![AgentId::new("agent-1")],
    )
}

struct SuccessfulPerceptionProvider;

impl ModelProvider for SuccessfulPerceptionProvider {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        let response = ModelResponse {
            id: "provider-receipt-1".into(),
            model: request.model,
            content: vec![ContentBlock::Text {
                text: "spoken words".into(),
            }],
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage {
                input_tokens: 4,
                output_tokens: 2,
                ..TokenUsage::default()
            },
        };
        Box::pin(async move {
            let stream: ModelEventStream = Box::pin(futures_util::stream::iter([Ok(
                ModelStreamEvent::Completed(Box::new(response)),
            )]));
            Ok(stream)
        })
    }
}

fn perception_governance() -> EvidenceGovernance {
    let encryption = EvidenceEncryption::from_secret("perception-test", &[9; 32]).unwrap();
    EvidenceGovernance::new("tenant-a", 30, encryption).unwrap()
}

fn audio_model() -> ModelInfo {
    ModelInfo {
        reference: ModelRef::new("test-provider", "audio-specialist"),
        context_window: 8_192,
        max_output_tokens: 512,
        capabilities: ModelCapabilities::AUDIO_INPUT,
    }
}

async fn running_perception_turn(store: &SqliteSessionStore, turn_id: &str) -> StoredSession {
    let mut session = make_session("sess-1", SessionLifetime::Persistent);
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    persist_turn_member(store, &session, &effective_config()).await;
    store
        .begin_turn(
            &turn_ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: turn_id.into(),
                agent_instance_id: turn_instance(),
                config_revision: 0,
                effective_config: effective_config(),
                user_content: serde_json::json!({"role":"user","content":"audio"}),
                model_id: "primary".into(),
            },
        )
        .await
        .unwrap();
    session
}

async fn perception_artifacts(
    evidence_path: &std::path::Path,
    turn_id: &str,
) -> Arc<dyn PerceptionArtifactStore> {
    let evidence = EvidenceStore::open_governed(evidence_path, perception_governance())
        .await
        .unwrap();
    RuntimeArtifactService::new(evidence)
        .unwrap()
        .bind_perception(ArtifactTurnBinding {
            user_id: "user-1".into(),
            agent_id: "agent-1".into(),
            session_id: "sess-1".into(),
            turn_id: turn_id.into(),
            created_at: crate::session::now_secs(),
        })
        .unwrap()
}

async fn prepare_completed_inference(
    store: &SqliteSessionStore,
    artifacts: &Arc<dyn PerceptionArtifactStore>,
    session: &StoredSession,
    turn_id: &str,
    invocation_id: &PerceptionInvocationId,
) -> (u64, ModelResponse) {
    store
        .begin_perception(PerceptionInvocationStart {
            session_id: session.id.clone(),
            turn_id: turn_id.into(),
            agent_instance_id: turn_instance(),
            invocation_id: invocation_id.clone(),
            modality: PerceptionModality::Audio,
            role: CognitiveRole::Audio,
            provider_id: "test-provider".into(),
            model_id: "audio-specialist".into(),
            recovery_policy: PerceptionRecoveryPolicy::RecoverFromReceipt,
            capability_revision: format!("sha256:{}", "a".repeat(64)),
            input_digest: format!("sha256:{}", "b".repeat(64)),
            input_bytes: 10,
        })
        .await
        .unwrap();
    let media = artifacts
        .persist_exact(
            invocation_id,
            PerceptionArtifactKind::SourceMedia,
            "audio/wav",
            b"RIFF-audio".to_vec(),
        )
        .await
        .unwrap();
    let revision = store
        .persist_perception_media(PerceptionMediaPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: 0,
            artifact_locator: media.locator,
        })
        .await
        .unwrap();
    let revision = store
        .advance_perception(PerceptionAdvance {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            expected_position: PerceptionExecutionPosition::MediaPersisted,
            next_position: PerceptionExecutionPosition::InferenceStarted,
        })
        .await
        .unwrap();
    let response = ModelResponse {
        id: format!("receipt-{turn_id}"),
        model: audio_model().reference,
        content: vec![ContentBlock::Text {
            text: "durable words".into(),
        }],
        stop_reason: StopReason::EndTurn,
        usage: TokenUsage::default(),
    };
    let receipt = artifacts
        .persist_exact(
            invocation_id,
            PerceptionArtifactKind::ProviderReceipt,
            "application/json",
            serde_json::to_vec(&response).unwrap(),
        )
        .await
        .unwrap();
    let revision = store
        .persist_perception_receipt(PerceptionReceiptPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            receipt_locator: receipt.locator,
        })
        .await
        .unwrap();
    (revision, response)
}

#[tokio::test]
async fn file_store_enforces_recovery_durability_controls() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = SqliteSessionStore::open(dir.path().join("sessions.db"))
        .await
        .unwrap();
    let connection = store.inner.conn.lock().await;
    let journal_mode = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
        .unwrap();
    let synchronous = connection
        .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let foreign_keys = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
        .unwrap();

    assert_eq!(journal_mode, "wal");
    assert_eq!(synchronous, SQLITE_SYNCHRONOUS_FULL);
    assert_eq!(foreign_keys, 1);
}

#[tokio::test]
async fn unknown_model_outcome_survives_restart_and_persists_manual_decision() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("model-crash.db");
    let invocation_id = ModelInvocationId::new();
    {
        let store = SqliteSessionStore::open(&path).await.unwrap();
        let mut session = make_session("model-crash", SessionLifetime::Persistent);
        session.effective_config = Some(effective_config());
        store.save(&session).await.unwrap();
        persist_turn_member(&store, &session, &effective_config()).await;
        store
            .begin_turn(
                &turn_ctx(),
                TurnStart {
                    session_id: session.id.clone(),
                    turn_id: "turn-crash".into(),
                    agent_instance_id: turn_instance(),
                    config_revision: 0,
                    effective_config: effective_config(),
                    user_content: serde_json::json!({"role":"user","content":"crash"}),
                    model_id: "model-a".into(),
                },
            )
            .await
            .unwrap();
        store
            .begin_model_iteration(ModelIterationStart {
                session_id: session.id,
                turn_id: "turn-crash".into(),
                iteration: 1,
                invocation_id: invocation_id.clone(),
                model_id: "model-a".into(),
                capability_revision: format!("sha256:{}", "a".repeat(64)),
                request_digest: format!("sha256:{}", "b".repeat(64)),
            })
            .await
            .unwrap();
    }

    let store = SqliteSessionStore::open(&path).await.unwrap();
    let interrupted = store.interrupted_model_iterations().await.unwrap();
    assert_eq!(interrupted.len(), 1);
    let classification = ModelRecoveryClassification::for_interrupted(
        interrupted[0].position,
        interrupted[0].response_message_id,
        interrupted[0].response_terminal,
    );
    assert_eq!(
        classification.decision,
        ModelRecoveryDecision::ManualReconciliation
    );
    store
        .classify_model_recovery(ModelRecoveryWrite {
            invocation_id: invocation_id.clone(),
            expected_revision: interrupted[0].ledger_revision,
            recovery_owner: "restart-test".into(),
            observed_at: 100,
            lease_expires_at: 130,
            classification,
        })
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteSessionStore::open(&path).await.unwrap();
    let persisted = reopened.interrupted_model_iterations().await.unwrap();
    assert_eq!(persisted[0].invocation_id, invocation_id);
    assert_eq!(
        persisted[0].recovery_reason,
        Some(ModelRecoveryReason::ProviderOutcomeUnknown)
    );
    assert!(persisted[0].operator_action_required);
    let action = ExecutionRecoveryActionWrite {
        action_id: ExecutionRecoveryActionId::new(),
        session_id: SessionId::new("model-crash"),
        turn_id: "turn-crash".into(),
        target: ExecutionRecoveryActionTarget::Model {
            invocation_id: invocation_id.clone(),
        },
        expected_ledger_revision: persisted[0].ledger_revision,
        action: ExecutionRecoveryAction::AbandonTurn,
        resolved_by: turn_instance(),
        rationale_digest: format!("sha256:{}", "c".repeat(64)),
        observed_at: 200,
        lease_expires_at: 230,
    };
    let receipt = reopened
        .resolve_execution_recovery(action.clone())
        .await
        .unwrap();
    assert_eq!(
        reopened.resolve_execution_recovery(action).await.unwrap(),
        receipt,
        "an exact operator retry must return the original durable receipt"
    );
    assert_eq!(
        reopened
            .turn(&SessionId::new("model-crash"), "turn-crash")
            .await
            .unwrap()
            .unwrap()
            .state,
        TurnState::Interrupted
    );
    assert!(
        reopened
            .interrupted_model_iterations()
            .await
            .unwrap()
            .is_empty()
    );
    let resolved = reopened
        .model_iterations(&SessionId::new("model-crash"), "turn-crash")
        .await
        .unwrap();
    assert_eq!(
        resolved[0].recovery_decision,
        Some(ModelRecoveryDecision::OperatorAbandoned)
    );
    assert!(!resolved[0].operator_action_required);
}

fn effective_config() -> sylvander_api::SessionEffectiveConfig {
    let source = sylvander_api::SessionConfigSource {
        kind: sylvander_api::SessionConfigSourceKind::AgentDefault,
        reference: Some("assistant@7".into()),
    };
    sylvander_api::SessionEffectiveConfig {
        agent_id: AgentId::new("agent-1"),
        agent_revision: 7,
        provider_id: "primary".into(),
        provider_revision: 1,
        model_id: "model-a".into(),
        model_revision: 1,
        reasoning_effort: sylvander_api::ReasoningEffort::Medium,
        permissions: sylvander_api::PermissionProfile::default(),
        prompt_profile: Some("coding".into()),
        system_prompt_sha256: "abc123".into(),
        prompt_manifest: sylvander_api::PromptManifest {
            layers: Vec::new(),
            aggregate_sha256: "manifest".into(),
            total_bytes: 0,
        },
        agent_workspace: Some(sylvander_api::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: "/agent".into(),
            read_only: false,
            instruction_focus: None,
        }),
        user_workspace: Some(sylvander_api::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: "/project".into(),
            read_only: false,
            instruction_focus: None,
        }),
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

// ---- session metadata ----

#[tokio::test]
async fn list_persistent_filters_correctly() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();
    store
        .save(&make_session("s2", SessionLifetime::Ephemeral))
        .await
        .unwrap();

    let persistent = store.list_persistent(false).await.unwrap();
    assert_eq!(persistent.len(), 1);
    assert_eq!(persistent[0].id, SessionId::new("s1"));
    assert_eq!(persistent[0].agents, vec![AgentId::new("agent-1")]);
}

#[tokio::test]
async fn save_and_get() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut session = make_session("s1", SessionLifetime::Persistent);
    session.config_revision = 3;
    session.config_overrides.model = Some(sylvander_api::ModelSelection {
        provider_id: "provider-a".into(),
        model_id: "model-a".into(),
    });
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    persist_turn_member(&store, &session, &effective_config()).await;

    let found = store.get(&SessionId::new("s1")).await.unwrap();
    assert!(found.is_some());
    let s = found.unwrap();
    assert_eq!(s.agents.len(), 1);
    assert_eq!(s.agents[0], AgentId::new("agent-1"));
    assert_eq!(s.config_revision, 3);
    assert_eq!(
        s.config_overrides.model,
        Some(sylvander_api::ModelSelection {
            provider_id: "provider-a".into(),
            model_id: "model-a".into(),
        })
    );
    assert_eq!(s.effective_config, session.effective_config);
}

#[tokio::test]
async fn opening_legacy_database_fails_closed_without_migration() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("legacy.db");
    {
        let connection = Connection::open(&path).unwrap();
        connection
                .execute_batch(
                    "CREATE TABLE sessions (\
                        id TEXT PRIMARY KEY, name TEXT NOT NULL, lifetime TEXT NOT NULL, \
                        workspace TEXT NOT NULL, user_id TEXT NOT NULL, created_at INTEGER NOT NULL, \
                        updated_at INTEGER NOT NULL, external_meta TEXT NOT NULL DEFAULT '{}', \
                        is_archived INTEGER NOT NULL DEFAULT 0, archive_reason TEXT\
                    );",
                )
                .unwrap();
    }

    assert!(matches!(
        SqliteSessionStore::open(&path).await,
        Err(SessionStoreError::IncompatibleSchema)
    ));
}

#[tokio::test]
async fn config_updates_are_optimistic_and_turn_start_is_atomic() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let session = make_session("s1", SessionLifetime::Persistent);
    store.save(&session).await.unwrap();
    persist_turn_member(&store, &session, &effective_config()).await;
    let effective = effective_config();
    let overrides = sylvander_api::SessionConfigOverrides {
        model: Some(sylvander_api::ModelSelection {
            provider_id: "primary".into(),
            model_id: "model-a".into(),
        }),
        ..Default::default()
    };

    let revision = store
        .update_config(&session.id, 0, overrides.clone(), effective.clone())
        .await
        .unwrap();
    assert_eq!(revision, 1);
    let conflict = store
        .update_config(&session.id, 0, overrides, effective.clone())
        .await
        .unwrap_err();
    assert!(matches!(
        conflict,
        SessionStoreError::ConfigConflict {
            expected: 0,
            actual: 1
        }
    ));

    let start = TurnStart {
        session_id: session.id.clone(),
        turn_id: "turn-1".into(),
        agent_instance_id: turn_instance(),
        config_revision: 1,
        effective_config: effective.clone(),
        user_content: serde_json::json!({"role": "user", "content": "hello"}),
        model_id: "model-a".into(),
    };
    let message = store.begin_turn(&turn_ctx(), start.clone()).await.unwrap();
    assert_eq!(message.seq, 0);
    let snapshot = store.turn(&session.id, "turn-1").await.unwrap().unwrap();
    assert_eq!(snapshot.agent_instance_id, turn_instance());
    assert_eq!(snapshot.config_revision, 1);
    assert_eq!(snapshot.effective_config, effective);
    assert_eq!(snapshot.state, TurnState::Running);
    assert_eq!(snapshot.ended_at, None);

    let mut unknown_instance = start.clone();
    unknown_instance.turn_id = "turn-unknown-instance".into();
    unknown_instance.agent_instance_id = AgentInstanceId::new("unknown");
    assert!(matches!(
        store
            .begin_turn(
                &ctx().with_agent_instance(AgentInstanceId::new("unknown")),
                unknown_instance,
            )
            .await
            .unwrap_err(),
        SessionStoreError::Invalid(_)
    ));

    let assistant = store
        .complete_turn(
            &turn_ctx(),
            TurnCompletion {
                session_id: session.id.clone(),
                turn_id: "turn-1".into(),
                assistant_content: serde_json::json!({"role": "assistant", "content": "done"}),
                model_id: "model-a".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(assistant.seq, 1);
    assert_eq!(assistant.role, MessageRole::Assistant);
    let completed = store.turn(&session.id, "turn-1").await.unwrap().unwrap();
    assert_eq!(completed.state, TurnState::Completed);
    assert!(completed.ended_at.is_some());
    assert_eq!(completed.failure_kind, None);
    assert!(
        store
            .complete_turn(
                &turn_ctx(),
                TurnCompletion {
                    session_id: session.id.clone(),
                    turn_id: "turn-1".into(),
                    assistant_content: serde_json::json!({"role": "assistant", "content": "again"}),
                    model_id: "model-a".into(),
                },
            )
            .await
            .is_err()
    );

    let mut failed_start = start.clone();
    failed_start.turn_id = "turn-2".into();
    failed_start.user_content = serde_json::json!({"role": "user", "content": "fail"});
    store.begin_turn(&turn_ctx(), failed_start).await.unwrap();
    store
        .finish_turn(
            &session.id,
            "turn-2",
            TurnState::Failed,
            Some(TurnFailureKind::AgentLoop),
        )
        .await
        .unwrap();
    let failed = store.turn(&session.id, "turn-2").await.unwrap().unwrap();
    assert_eq!(failed.state, TurnState::Failed);
    assert_eq!(failed.failure_kind, Some(TurnFailureKind::AgentLoop));

    assert!(store.begin_turn(&turn_ctx(), start).await.is_err());
    let stale = TurnStart {
        session_id: session.id.clone(),
        turn_id: "turn-stale".into(),
        agent_instance_id: turn_instance(),
        config_revision: 0,
        effective_config: effective_config(),
        user_content: serde_json::json!({"role": "user", "content": "stale"}),
        model_id: "model-a".into(),
    };
    assert!(matches!(
        store.begin_turn(&turn_ctx(), stale).await,
        Err(SessionStoreError::ConfigConflict { .. })
    ));
    assert!(
        store
            .turn(&session.id, "turn-stale")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .read_history(&turn_ctx(), &session.id, false, None)
            .await
            .unwrap()
            .len(),
        3
    );
}

#[tokio::test]
async fn model_iteration_facts_are_atomic_sequential_and_recoverable() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let session = make_session("model-ledger", SessionLifetime::Persistent);
    store.save(&session).await.unwrap();
    persist_turn_member(&store, &session, &effective_config()).await;
    let effective = effective_config();
    store
        .update_config(
            &session.id,
            0,
            sylvander_api::SessionConfigOverrides::default(),
            effective.clone(),
        )
        .await
        .unwrap();
    store
        .begin_turn(
            &turn_ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-model-ledger".into(),
                agent_instance_id: turn_instance(),
                config_revision: 1,
                effective_config: effective,
                user_content: serde_json::json!({"role":"user","content":"recover"}),
                model_id: "model-a".into(),
            },
        )
        .await
        .unwrap();

    let invocation_id = ModelInvocationId::new();
    store
        .begin_model_iteration(ModelIterationStart {
            session_id: session.id.clone(),
            turn_id: "turn-model-ledger".into(),
            iteration: 1,
            invocation_id: invocation_id.clone(),
            model_id: "model-a".into(),
            capability_revision: format!("sha256:{}", "a".repeat(64)),
            request_digest: format!("sha256:{}", "b".repeat(64)),
        })
        .await
        .unwrap();

    let interrupted = store.interrupted_model_iterations().await.unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(
        interrupted[0].position,
        ModelExecutionPosition::ModelStarted
    );
    let unknown = ModelRecoveryClassification::for_interrupted(
        interrupted[0].position,
        interrupted[0].response_message_id,
        interrupted[0].response_terminal,
    );
    assert_eq!(
        unknown.decision,
        ModelRecoveryDecision::ManualReconciliation
    );

    let commit = store
        .persist_model_response(
            &turn_ctx(),
            ModelResponsePersistence {
                invocation_id: invocation_id.clone(),
                expected_revision: 0,
                assistant_content: serde_json::json!({
                    "role":"assistant",
                    "content":[{"type":"tool_use","id":"call-1","name":"read","input":{}}]
                }),
                model_id: "model-a".into(),
                terminal: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(commit.ledger_revision, 1);
    assert_eq!(commit.message.role, MessageRole::Assistant);

    let persisted = store
        .model_iterations(&session.id, "turn-model-ledger")
        .await
        .unwrap();
    assert_eq!(persisted[0].response_message_id, Some(commit.message.id));
    assert_eq!(persisted[0].response_terminal, Some(false));
    assert_eq!(
        persisted[0].position,
        ModelExecutionPosition::ResponsePersisted
    );

    let revision = store
        .advance_model_iteration(ModelIterationAdvance {
            invocation_id: invocation_id.clone(),
            expected_revision: 1,
            expected_position: ModelExecutionPosition::ResponsePersisted,
            next_position: ModelExecutionPosition::ToolsResolved,
        })
        .await
        .unwrap();
    assert_eq!(revision, 2);
    assert!(
        store
            .advance_model_iteration(ModelIterationAdvance {
                invocation_id,
                expected_revision: 1,
                expected_position: ModelExecutionPosition::ResponsePersisted,
                next_position: ModelExecutionPosition::ToolsResolved,
            })
            .await
            .is_err()
    );

    let terminal_id = ModelInvocationId::new();
    store
        .begin_model_iteration(ModelIterationStart {
            session_id: session.id.clone(),
            turn_id: "turn-model-ledger".into(),
            iteration: 2,
            invocation_id: terminal_id.clone(),
            model_id: "model-a".into(),
            capability_revision: format!("sha256:{}", "a".repeat(64)),
            request_digest: format!("sha256:{}", "c".repeat(64)),
        })
        .await
        .unwrap();
    let terminal = store
        .persist_model_response(
            &turn_ctx(),
            ModelResponsePersistence {
                invocation_id: terminal_id.clone(),
                expected_revision: 0,
                assistant_content: serde_json::json!({"role":"assistant","content":"done"}),
                model_id: "model-a".into(),
                terminal: true,
            },
        )
        .await
        .unwrap();
    let classification = ModelRecoveryClassification::for_interrupted(
        ModelExecutionPosition::ResponsePersisted,
        Some(terminal.message.id),
        Some(true),
    );
    let classified_revision = store
        .classify_model_recovery(ModelRecoveryWrite {
            invocation_id: terminal_id.clone(),
            expected_revision: terminal.ledger_revision,
            recovery_owner: "test-recovery".into(),
            observed_at: 100,
            lease_expires_at: 130,
            classification,
        })
        .await
        .unwrap();
    let completed = store
        .complete_persisted_turn(PersistedTurnCompletion {
            invocation_id: terminal_id,
            expected_revision: classified_revision,
        })
        .await
        .unwrap();
    assert_eq!(completed.id, terminal.message.id);
    assert_eq!(
        store
            .turn(&session.id, "turn-model-ledger")
            .await
            .unwrap()
            .unwrap()
            .state,
        TurnState::Completed
    );
}

#[tokio::test]
async fn metadata_patch_cannot_roll_back_a_prompt_config_update() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut session = make_session("s1", SessionLifetime::Persistent);
    session
        .external_meta
        .insert("existing".into(), serde_json::json!("kept"));
    store.save(&session).await.unwrap();
    let stale = store.get(&session.id).await.unwrap().unwrap();

    let mut effective = effective_config();
    effective.system_prompt_sha256 = "new-prompt-hash".into();
    let overrides = sylvander_api::SessionConfigOverrides {
        system_prompt: Some("new prompt".into()),
        ..Default::default()
    };
    store
        .update_config(&session.id, 0, overrides.clone(), effective.clone())
        .await
        .unwrap();

    let external_meta =
        std::collections::HashMap::from([("channel".into(), serde_json::json!("telegram"))]);
    store
        .patch_metadata(
            &session.id,
            SessionMetadataPatch {
                name: Some(format!("{} renamed", stale.name)),
                external_meta,
            },
        )
        .await
        .unwrap();

    let loaded = store.get(&session.id).await.unwrap().unwrap();
    assert_eq!(loaded.name, "session-s1 renamed");
    assert_eq!(loaded.external_meta["existing"], "kept");
    assert_eq!(loaded.external_meta["channel"], "telegram");
    assert_eq!(loaded.config_revision, 1);
    assert_eq!(loaded.config_overrides, overrides);
    assert_eq!(loaded.effective_config, Some(effective));
}

#[tokio::test]
async fn tool_lifecycle_is_bound_to_one_running_turn_and_one_terminal() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut session = make_session("sess-1", SessionLifetime::Persistent);
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    persist_turn_member(&store, &session, &effective_config()).await;
    store
        .begin_turn(
            &turn_ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-tools".into(),
                agent_instance_id: turn_instance(),
                config_revision: 0,
                effective_config: effective_config(),
                user_content: serde_json::json!({"role": "user", "content": "inspect"}),
                model_id: "model-a".into(),
            },
        )
        .await
        .unwrap();

    let start = ToolCallStart {
        session_id: session.id.clone(),
        turn_id: "turn-tools".into(),
        call_id: "call-1".into(),
        invocation_id: ToolInvocationId::new(),
        tool_name: "Read".into(),
        invocation_class: Some(ToolInvocationClass::Read),
        declared_recovery_policy: ToolRecoveryPolicy::NeverReplay,
        effective_recovery_policy: ToolRecoveryPolicy::NeverReplay,
        capability_revision: "sha256:test-surface".into(),
        input_digest: "sha256:test-input".into(),
    };
    store.begin_tool_call(start.clone()).await.unwrap();
    store.begin_tool_call(start.clone()).await.unwrap();
    let mut conflicting = start.clone();
    conflicting.input_digest = "sha256:different-input".into();
    assert!(store.begin_tool_call(conflicting).await.is_err());
    let calls = store.tool_calls(&session.id, "turn-tools").await.unwrap();
    assert_eq!(calls[0].invocation_id, start.invocation_id);
    assert_eq!(calls[0].invocation_class, Some(ToolInvocationClass::Read));
    assert_eq!(calls[0].position, ToolExecutionPosition::Prepared);
    assert_eq!(calls[0].ledger_revision, 0);

    let advance = ToolCallAdvance {
        session_id: session.id.clone(),
        turn_id: "turn-tools".into(),
        call_id: "call-1".into(),
        expected_revision: 0,
        expected_position: ToolExecutionPosition::Prepared,
        next_position: ToolExecutionPosition::Authorized,
    };
    assert_eq!(store.advance_tool_call(advance.clone()).await.unwrap(), 1);
    assert_eq!(store.advance_tool_call(advance).await.unwrap(), 1);
    assert!(
        store
            .advance_tool_call(ToolCallAdvance {
                session_id: session.id.clone(),
                turn_id: "turn-tools".into(),
                call_id: "call-1".into(),
                expected_revision: 1,
                expected_position: ToolExecutionPosition::Authorized,
                next_position: ToolExecutionPosition::ResultPersisted,
            })
            .await
            .is_err()
    );
    assert!(
        store
            .complete_turn(
                &turn_ctx(),
                TurnCompletion {
                    session_id: session.id.clone(),
                    turn_id: "turn-tools".into(),
                    assistant_content: serde_json::json!({"role": "assistant", "content": "early"}),
                    model_id: "model-a".into(),
                },
            )
            .await
            .is_err()
    );
    store
        .finish_tool_call(ToolCallCompletion {
            session_id: session.id.clone(),
            turn_id: "turn-tools".into(),
            call_id: "call-1".into(),
            state: ToolCallState::Failed,
            failure_kind: Some(ToolCallFailureKind::FilesystemBoundaryPolicyViolation),
        })
        .await
        .unwrap();
    assert!(
        store
            .finish_tool_call(ToolCallCompletion {
                session_id: session.id.clone(),
                turn_id: "turn-tools".into(),
                call_id: "call-1".into(),
                state: ToolCallState::Succeeded,
                failure_kind: None,
            })
            .await
            .is_err()
    );

    let calls = store.tool_calls(&session.id, "turn-tools").await.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].state, ToolCallState::Failed);
    assert_eq!(
        calls[0].failure_kind,
        Some(ToolCallFailureKind::FilesystemBoundaryPolicyViolation)
    );
    assert!(calls[0].ended_at.is_some());

    store
        .begin_tool_call(ToolCallStart {
            session_id: session.id.clone(),
            turn_id: "turn-tools".into(),
            call_id: "call-2".into(),
            invocation_id: ToolInvocationId::new(),
            tool_name: "Command".into(),
            invocation_class: Some(ToolInvocationClass::Terminal),
            declared_recovery_policy: ToolRecoveryPolicy::NeverReplay,
            effective_recovery_policy: ToolRecoveryPolicy::NeverReplay,
            capability_revision: "sha256:test-surface".into(),
            input_digest: "sha256:test-input-2".into(),
        })
        .await
        .unwrap();
    store
        .finish_turn(&session.id, "turn-tools", TurnState::Interrupted, None)
        .await
        .unwrap();
    let calls = store.tool_calls(&session.id, "turn-tools").await.unwrap();
    assert_eq!(calls[1].state, ToolCallState::Abandoned);
    assert!(calls[1].ended_at.is_some());
}

#[tokio::test]
async fn interrupted_calls_are_classified_under_a_bounded_lease() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut session = make_session("sess-1", SessionLifetime::Persistent);
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    persist_turn_member(&store, &session, &effective_config()).await;
    store
        .begin_turn(
            &turn_ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-recovery".into(),
                agent_instance_id: turn_instance(),
                config_revision: 0,
                effective_config: effective_config(),
                user_content: serde_json::json!({"role": "user", "content": "mutate"}),
                model_id: "model-a".into(),
            },
        )
        .await
        .unwrap();
    let invocation_id = ToolInvocationId::new();
    store
        .begin_tool_call(ToolCallStart {
            session_id: session.id.clone(),
            turn_id: "turn-recovery".into(),
            call_id: "call-recovery".into(),
            invocation_id: invocation_id.clone(),
            tool_name: "Write".into(),
            invocation_class: Some(ToolInvocationClass::FilesystemMutation),
            declared_recovery_policy: ToolRecoveryPolicy::ReconcileBeforeRetry,
            effective_recovery_policy: ToolRecoveryPolicy::NeverReplay,
            capability_revision: "sha256:recovery-surface".into(),
            input_digest: "sha256:recovery-input".into(),
        })
        .await
        .unwrap();

    let interrupted = store.interrupted_tool_calls().await.unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].invocation_id, invocation_id);
    let classification = super::super::RecoveryClassification::for_interrupted(
        interrupted[0].position,
        interrupted[0].effective_recovery_policy,
    );
    let write = ToolRecoveryWrite {
        invocation_id: invocation_id.clone(),
        expected_revision: 0,
        recovery_owner: "runtime-a".into(),
        observed_at: 100,
        lease_expires_at: 200,
        classification,
    };
    assert_eq!(
        store.classify_tool_recovery(write.clone()).await.unwrap(),
        1
    );
    let mut competing = write.clone();
    competing.expected_revision = 1;
    competing.recovery_owner = "runtime-b".into();
    competing.observed_at = 150;
    competing.lease_expires_at = 250;
    assert!(
        store
            .classify_tool_recovery(competing.clone())
            .await
            .is_err()
    );

    competing.expected_revision = 1;
    competing.observed_at = 201;
    assert_eq!(store.classify_tool_recovery(competing).await.unwrap(), 2);
    let recovered = store
        .tool_calls(&session.id, "turn-recovery")
        .await
        .unwrap();
    assert_eq!(
        recovered[0].recovery_decision,
        Some(ToolRecoveryDecision::ResumeAuthorization),
    );
    assert_eq!(recovered[0].recovery_attempts, 2);
    assert_eq!(recovered[0].recovery_owner.as_deref(), Some("runtime-b"));
    assert_eq!(recovered[0].first_interrupted_at, Some(100));

    assert!(
        store
            .begin_turn(
                &turn_ctx(),
                TurnStart {
                    session_id: session.id.clone(),
                    turn_id: "turn-must-wait".into(),
                    agent_instance_id: turn_instance(),
                    config_revision: 0,
                    effective_config: effective_config(),
                    user_content: serde_json::json!({"role": "user", "content": "next"}),
                    model_id: "model-a".into(),
                },
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn tool_result_and_result_position_commit_atomically() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut session = make_session("sess-1", SessionLifetime::Persistent);
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    persist_turn_member(&store, &session, &effective_config()).await;
    store
        .begin_turn(
            &turn_ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-result".into(),
                agent_instance_id: turn_instance(),
                config_revision: 0,
                effective_config: effective_config(),
                user_content: serde_json::json!({"role": "user", "content": "read"}),
                model_id: "model-a".into(),
            },
        )
        .await
        .unwrap();
    store
        .begin_tool_call(ToolCallStart {
            session_id: session.id.clone(),
            turn_id: "turn-result".into(),
            call_id: "call-result".into(),
            invocation_id: ToolInvocationId::new(),
            tool_name: "Read".into(),
            invocation_class: Some(ToolInvocationClass::Read),
            declared_recovery_policy: ToolRecoveryPolicy::NeverReplay,
            effective_recovery_policy: ToolRecoveryPolicy::NeverReplay,
            capability_revision: "sha256:result-surface".into(),
            input_digest: "sha256:result-input".into(),
        })
        .await
        .unwrap();
    let mut revision = 0;
    for (current, next) in [
        (
            ToolExecutionPosition::Prepared,
            ToolExecutionPosition::Authorized,
        ),
        (
            ToolExecutionPosition::Authorized,
            ToolExecutionPosition::EffectStarted,
        ),
        (
            ToolExecutionPosition::EffectStarted,
            ToolExecutionPosition::EffectCommitted,
        ),
    ] {
        revision = store
            .advance_tool_call(ToolCallAdvance {
                session_id: session.id.clone(),
                turn_id: "turn-result".into(),
                call_id: "call-result".into(),
                expected_revision: revision,
                expected_position: current,
                next_position: next,
            })
            .await
            .unwrap();
    }
    let content = serde_json::to_value(sylvander_llm_core::ChatMessage::user_blocks(vec![
        sylvander_llm_core::ContentBlock::tool_result_text("call-result", "value", false),
    ]))
    .unwrap();
    assert_eq!(
        store
            .persist_tool_result(
                &turn_ctx().with_trace_id("turn-result"),
                ToolResultPersistence {
                    session_id: session.id.clone(),
                    turn_id: "turn-result".into(),
                    call_id: "call-result".into(),
                    expected_revision: revision,
                    expected_position: ToolExecutionPosition::EffectCommitted,
                    content: content.clone(),
                    tool_name: "Read".into(),
                    terminal_state: ToolCallState::Succeeded,
                    failure_kind: None,
                },
            )
            .await
            .unwrap(),
        4,
    );
    let calls = store.tool_calls(&session.id, "turn-result").await.unwrap();
    assert_eq!(calls[0].position, ToolExecutionPosition::ResultPersisted);
    assert_eq!(calls[0].state, ToolCallState::Succeeded);
    assert!(calls[0].ended_at.is_some());
    let history = store
        .read_history(&turn_ctx(), &session.id, false, None)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].role, MessageRole::Tool);
    assert_eq!(history[1].content, content);
    assert!(
        store
            .persist_tool_result(
                &turn_ctx(),
                ToolResultPersistence {
                    session_id: session.id,
                    turn_id: "turn-result".into(),
                    call_id: "call-result".into(),
                    expected_revision: revision,
                    expected_position: ToolExecutionPosition::EffectCommitted,
                    content: serde_json::json!({}),
                    tool_name: "Read".into(),
                    terminal_state: ToolCallState::Succeeded,
                    failure_kind: None,
                },
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn boot_recovery_persists_manual_decision_and_observes_it() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut session = make_session("sess-1", SessionLifetime::Persistent);
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    persist_turn_member(&store, &session, &effective_config()).await;
    store
        .begin_turn(
            &turn_ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-crashed".into(),
                agent_instance_id: turn_instance(),
                config_revision: 0,
                effective_config: effective_config(),
                user_content: serde_json::json!({"role": "user", "content": "send"}),
                model_id: "model-a".into(),
            },
        )
        .await
        .unwrap();
    store
        .begin_tool_call(ToolCallStart {
            session_id: session.id.clone(),
            turn_id: "turn-crashed".into(),
            call_id: "call-send".into(),
            invocation_id: ToolInvocationId::new(),
            tool_name: "Send".into(),
            invocation_class: Some(ToolInvocationClass::Extension),
            declared_recovery_policy: ToolRecoveryPolicy::NeverReplay,
            effective_recovery_policy: ToolRecoveryPolicy::NeverReplay,
            capability_revision: "sha256:send-surface".into(),
            input_digest: "sha256:send-input".into(),
        })
        .await
        .unwrap();
    for (revision, current, next) in [
        (
            0,
            ToolExecutionPosition::Prepared,
            ToolExecutionPosition::Authorized,
        ),
        (
            1,
            ToolExecutionPosition::Authorized,
            ToolExecutionPosition::EffectStarted,
        ),
    ] {
        store
            .advance_tool_call(ToolCallAdvance {
                session_id: session.id.clone(),
                turn_id: "turn-crashed".into(),
                call_id: "call-send".into(),
                expected_revision: revision,
                expected_position: current,
                next_position: next,
            })
            .await
            .unwrap();
    }
    let observability = crate::observability::RuntimeObservability::new();
    let summary = crate::agent::run::recovery::classify_interrupted_tool_calls(
        std::sync::Arc::new(store.clone()),
        &observability,
        None,
        1_000,
    )
    .await
    .unwrap();
    assert_eq!(summary.discovered, 1);
    assert_eq!(summary.classified, 1);
    assert_eq!(summary.manual_reconciliation, 1);
    let calls = store.tool_calls(&session.id, "turn-crashed").await.unwrap();
    assert_eq!(
        calls[0].recovery_decision,
        Some(ToolRecoveryDecision::ManualReconciliation),
    );
    assert!(calls[0].operator_action_required);
    let action = ExecutionRecoveryActionWrite {
        action_id: ExecutionRecoveryActionId::new(),
        session_id: session.id.clone(),
        turn_id: "turn-crashed".into(),
        target: ExecutionRecoveryActionTarget::Tool {
            invocation_id: calls[0].invocation_id.clone(),
        },
        expected_ledger_revision: calls[0].ledger_revision,
        action: ExecutionRecoveryAction::ConfirmNoEffectAndRetry,
        resolved_by: turn_instance(),
        rationale_digest: format!("sha256:{}", "d".repeat(64)),
        observed_at: 1_001,
        lease_expires_at: 1_031,
    };
    let receipt = store
        .resolve_execution_recovery(action.clone())
        .await
        .unwrap();
    assert_eq!(
        store.resolve_execution_recovery(action).await.unwrap(),
        receipt
    );
    let resolved = store.tool_calls(&session.id, "turn-crashed").await.unwrap();
    assert_eq!(
        resolved[0].recovery_decision,
        Some(ToolRecoveryDecision::RetrySameInvocation)
    );
    assert_eq!(
        resolved[0].recovery_reason,
        Some(ToolRecoveryReason::OperatorConfirmedNoEffect)
    );
    assert!(!resolved[0].operator_action_required);
    let deferred = crate::agent::run::recovery::classify_interrupted_tool_calls(
        std::sync::Arc::new(store.clone()),
        &observability,
        None,
        1_002,
    )
    .await
    .unwrap();
    assert_eq!(deferred.lease_deferred, 1);
    let reacquired = crate::agent::run::recovery::classify_interrupted_tool_calls(
        std::sync::Arc::new(store.clone()),
        &observability,
        None,
        1_031,
    )
    .await
    .unwrap();
    assert_eq!(reacquired.classified, 1);
    assert_eq!(reacquired.manual_reconciliation, 0);
    let recovered = store.tool_calls(&session.id, "turn-crashed").await.unwrap();
    assert_eq!(
        recovered[0].recovery_decision,
        Some(ToolRecoveryDecision::RetrySameInvocation)
    );
    assert_eq!(
        recovered[0].recovery_reason,
        Some(ToolRecoveryReason::OperatorConfirmedNoEffect)
    );
    assert_ne!(recovered[0].recovery_owner, resolved[0].recovery_owner);
    let observed = observability.snapshot();
    assert_eq!(observed.tool_recoveries_classified, 3);
    assert_eq!(observed.tool_recoveries_manual, 1);
}

#[tokio::test]
async fn boot_recovery_reconciles_committed_workspace_effect_without_replay() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut session = make_session("sess-1", SessionLifetime::Persistent);
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    persist_turn_member(&store, &session, &effective_config()).await;
    store
        .begin_turn(
            &turn_ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-write".into(),
                agent_instance_id: turn_instance(),
                config_revision: 0,
                effective_config: effective_config(),
                user_content: serde_json::json!({"role": "user", "content": "write"}),
                model_id: "model-a".into(),
            },
        )
        .await
        .unwrap();
    let invocation_id = ToolInvocationId::new();
    store
        .begin_tool_call(ToolCallStart {
            session_id: session.id.clone(),
            turn_id: "turn-write".into(),
            call_id: "call-write".into(),
            invocation_id,
            tool_name: "Write".into(),
            invocation_class: Some(ToolInvocationClass::FilesystemMutation),
            declared_recovery_policy: ToolRecoveryPolicy::ReconcileBeforeRetry,
            effective_recovery_policy: ToolRecoveryPolicy::ReconcileBeforeRetry,
            capability_revision: "sha256:write-surface".into(),
            input_digest: "sha256:write-input".into(),
        })
        .await
        .unwrap();
    for (revision, current, next) in [
        (
            0,
            ToolExecutionPosition::Prepared,
            ToolExecutionPosition::Authorized,
        ),
        (
            1,
            ToolExecutionPosition::Authorized,
            ToolExecutionPosition::EffectStarted,
        ),
    ] {
        store
            .advance_tool_call(ToolCallAdvance {
                session_id: session.id.clone(),
                turn_id: "turn-write".into(),
                call_id: "call-write".into(),
                expected_revision: revision,
                expected_position: current,
                next_position: next,
            })
            .await
            .unwrap();
    }
    let workspace = tempfile::tempdir().unwrap();
    let journal_root = tempfile::tempdir().unwrap();
    let journal = WorkspaceJournal::new(journal_root.path());
    let prepared = journal
        .prepare(
            "sess-1",
            "turn-write",
            "call-write",
            workspace.path(),
            "result.txt",
            b"committed once",
        )
        .unwrap();
    std::fs::write(workspace.path().join("result.txt"), "committed once").unwrap();
    journal.commit(&prepared).unwrap();

    let observability = crate::observability::RuntimeObservability::new();
    let summary = crate::agent::run::recovery::classify_interrupted_tool_calls(
        std::sync::Arc::new(store.clone()),
        &observability,
        Some(&journal),
        1_000,
    )
    .await
    .unwrap();
    assert_eq!(summary.manual_reconciliation, 1);
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("result.txt")).unwrap(),
        "committed once"
    );
    let calls = store.tool_calls(&session.id, "turn-write").await.unwrap();
    assert_eq!(calls[0].position, ToolExecutionPosition::EffectCommitted);
    assert_eq!(
        calls[0].recovery_decision,
        Some(ToolRecoveryDecision::ManualReconciliation),
    );
}

#[tokio::test]
async fn perception_positions_recover_from_receipt_and_survive_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("perception.sqlite3");
    let store = SqliteSessionStore::open(&path).await.unwrap();
    let mut session = make_session("sess-1", SessionLifetime::Persistent);
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    persist_turn_member(&store, &session, &effective_config()).await;
    store
        .begin_turn(
            &turn_ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-perception".into(),
                agent_instance_id: turn_instance(),
                config_revision: 0,
                effective_config: effective_config(),
                user_content: serde_json::json!({"role":"user","content":"inspect image"}),
                model_id: "primary".into(),
            },
        )
        .await
        .unwrap();
    let invocation_id = PerceptionInvocationId::new();
    store
        .begin_perception(PerceptionInvocationStart {
            session_id: session.id.clone(),
            turn_id: "turn-perception".into(),
            agent_instance_id: turn_instance(),
            invocation_id: invocation_id.clone(),
            modality: crate::agent::perception::PerceptionModality::Image,
            role: crate::agent::cognition::CognitiveRole::Vision,
            provider_id: "provider".into(),
            model_id: "vision".into(),
            recovery_policy: PerceptionRecoveryPolicy::RecoverFromReceipt,
            capability_revision: format!("sha256:{}", "a".repeat(64)),
            input_digest: format!("sha256:{}", "b".repeat(64)),
            input_bytes: 1024,
        })
        .await
        .unwrap();
    let media_locator = format!("artifact:{}", uuid::Uuid::new_v4());
    assert_eq!(
        store
            .persist_perception_media(PerceptionMediaPersistence {
                invocation_id: invocation_id.clone(),
                expected_revision: 0,
                artifact_locator: media_locator.clone(),
            })
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .advance_perception(PerceptionAdvance {
                invocation_id: invocation_id.clone(),
                expected_revision: 1,
                expected_position: PerceptionExecutionPosition::MediaPersisted,
                next_position: PerceptionExecutionPosition::InferenceStarted,
            })
            .await
            .unwrap(),
        2
    );
    let interrupted = store.interrupted_perception_invocations().await.unwrap();
    assert_eq!(interrupted.len(), 1);
    let observability = crate::observability::RuntimeObservability::new();
    let summary = crate::agent::run::recovery::classify_interrupted_tool_calls(
        std::sync::Arc::new(store.clone()),
        &observability,
        None,
        1_000,
    )
    .await
    .unwrap();
    assert_eq!(summary.perception_discovered, 1);
    assert_eq!(summary.perception_classified, 1);
    let classified = store.interrupted_perception_invocations().await.unwrap();
    assert_eq!(
        classified[0].recovery_decision,
        Some(PerceptionRecoveryDecision::RecoverReceipt)
    );
    assert_eq!(observability.snapshot().perception_recoveries_classified, 1);
    let receipt_locator = format!("artifact:{}", uuid::Uuid::new_v4());
    store
        .persist_perception_receipt(PerceptionReceiptPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: 3,
            receipt_locator: receipt_locator.clone(),
        })
        .await
        .unwrap();
    let output_locator = format!("artifact:{}", uuid::Uuid::new_v4());
    store
        .persist_perception_artifact(PerceptionArtifactPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: 4,
            artifact_locator: output_locator.clone(),
            output_digest: format!("sha256:{}", "c".repeat(64)),
        })
        .await
        .unwrap();
    assert_eq!(
        store.complete_perception(&invocation_id, 5).await.unwrap(),
        6
    );
    assert!(
        store
            .interrupted_perception_invocations()
            .await
            .unwrap()
            .is_empty()
    );
    drop(store);

    let reopened = SqliteSessionStore::open(path).await.unwrap();
    let invocations = reopened
        .perception_invocations(&session.id, "turn-perception")
        .await
        .unwrap();
    let [snapshot] = invocations.as_slice() else {
        panic!("one durable perception invocation must survive restart");
    };
    assert_eq!(
        snapshot.position,
        PerceptionExecutionPosition::ResultPersisted
    );
    assert_eq!(
        snapshot.media_artifact_locator.as_deref(),
        Some(media_locator.as_str())
    );
    assert_eq!(
        snapshot.receipt_locator.as_deref(),
        Some(receipt_locator.as_str())
    );
    assert_eq!(
        snapshot.output_artifact_locator.as_deref(),
        Some(output_locator.as_str())
    );
}

#[tokio::test]
async fn terminal_perception_failure_survives_restart_without_recovery_work() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("perception-failed.sqlite3");
    let store = SqliteSessionStore::open(&path).await.unwrap();
    let session = running_perception_turn(&store, "turn-perception-failed").await;
    let artifacts = perception_artifacts(
        &directory.path().join("failed-evidence.sqlite"),
        "turn-perception-failed",
    )
    .await;
    let invocation_id = PerceptionInvocationId::new();
    store
        .begin_perception(PerceptionInvocationStart {
            session_id: session.id.clone(),
            turn_id: "turn-perception-failed".into(),
            agent_instance_id: turn_instance(),
            invocation_id: invocation_id.clone(),
            modality: PerceptionModality::Audio,
            role: CognitiveRole::Audio,
            provider_id: "test-provider".into(),
            model_id: "audio-specialist".into(),
            recovery_policy: PerceptionRecoveryPolicy::RecoverFromReceipt,
            capability_revision: format!("sha256:{}", "a".repeat(64)),
            input_digest: format!("sha256:{}", "b".repeat(64)),
            input_bytes: 4,
        })
        .await
        .unwrap();
    let media = artifacts
        .persist_exact(
            &invocation_id,
            PerceptionArtifactKind::SourceMedia,
            "audio/wav",
            b"RIFF".to_vec(),
        )
        .await
        .unwrap();
    let revision = store
        .persist_perception_media(PerceptionMediaPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: 0,
            artifact_locator: media.locator,
        })
        .await
        .unwrap();
    let revision = store
        .advance_perception(PerceptionAdvance {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            expected_position: PerceptionExecutionPosition::MediaPersisted,
            next_position: PerceptionExecutionPosition::InferenceStarted,
        })
        .await
        .unwrap();
    store
        .fail_perception(PerceptionFailurePersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            failure_kind: PerceptionFailureKind::Provider,
        })
        .await
        .unwrap();
    drop(store);

    let reopened = SqliteSessionStore::open(path).await.unwrap();
    let invocations = reopened
        .perception_invocations(&session.id, "turn-perception-failed")
        .await
        .unwrap();
    assert_eq!(invocations[0].position, PerceptionExecutionPosition::Failed);
    assert_eq!(
        invocations[0].failure_kind,
        Some(PerceptionFailureKind::Provider)
    );
    assert!(
        reopened
            .interrupted_perception_invocations()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        reopened
            .perception_session_summary(&session.id)
            .await
            .unwrap(),
        PerceptionSessionSummary {
            invocations: 1,
            completed: 1,
            interrupted: 0,
            operator_action_required: 0,
        }
    );
}

#[tokio::test]
async fn specialist_execution_commits_receipt_artifact_and_model_visible_result() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteSessionStore::open(directory.path().join("sessions.sqlite"))
            .await
            .unwrap(),
    );
    let session = running_perception_turn(&store, "turn-specialist").await;
    let artifacts =
        perception_artifacts(&directory.path().join("evidence.sqlite"), "turn-specialist").await;
    let invocation_id = PerceptionInvocationId::new();
    let bytes = b"RIFF-audio".to_vec();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let result = execute_perception(
        store.clone(),
        artifacts.clone(),
        Arc::new(SuccessfulPerceptionProvider),
        PerceptionExecutionRequest {
            session_id: session.id.clone(),
            turn_id: "turn-specialist".into(),
            agent_instance_id: turn_instance(),
            invocation_id: invocation_id.clone(),
            modality: PerceptionModality::Audio,
            role: CognitiveRole::Audio,
            model: audio_model(),
            recovery_policy: PerceptionRecoveryPolicy::RecoverFromReceipt,
            media_type: "audio/wav".into(),
            media_bytes: bytes,
            media_block: ContentBlock::Audio {
                audio: AudioContent {
                    data: encoded,
                    format: AudioFormat::Wav,
                    transcript: None,
                },
            },
        },
    )
    .await
    .unwrap();

    assert_eq!(result.text, "spoken words");
    assert_eq!(result.provider_response_id, "provider-receipt-1");
    assert_eq!(result.usage.input_tokens, 4);
    let invocations = store
        .perception_invocations(&session.id, "turn-specialist")
        .await
        .unwrap();
    assert_eq!(
        invocations[0].position,
        PerceptionExecutionPosition::ResultPersisted
    );
    assert_eq!(
        invocations[0].receipt_locator.as_deref(),
        Some(
            artifacts
                .load_exact(&invocation_id, PerceptionArtifactKind::ProviderReceipt)
                .await
                .unwrap()
                .unwrap()
                .locator
                .as_str()
        )
    );
    assert_eq!(
        invocations[0].output_digest.as_deref(),
        Some(result.output_digest.as_str())
    );
    assert_eq!(
        store.perception_session_summary(&session.id).await.unwrap(),
        PerceptionSessionSummary {
            invocations: 1,
            completed: 1,
            interrupted: 0,
            operator_action_required: 0,
        }
    );
}

#[tokio::test]
async fn receipt_written_before_ledger_advance_recovers_without_provider_replay() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteSessionStore::open(directory.path().join("sessions.sqlite"))
            .await
            .unwrap(),
    );
    let session = running_perception_turn(&store, "turn-receipt-recovery").await;
    let artifacts = perception_artifacts(
        &directory.path().join("evidence.sqlite"),
        "turn-receipt-recovery",
    )
    .await;
    let invocation_id = PerceptionInvocationId::new();
    store
        .begin_perception(PerceptionInvocationStart {
            session_id: session.id.clone(),
            turn_id: "turn-receipt-recovery".into(),
            agent_instance_id: turn_instance(),
            invocation_id: invocation_id.clone(),
            modality: PerceptionModality::Audio,
            role: CognitiveRole::Audio,
            provider_id: "test-provider".into(),
            model_id: "audio-specialist".into(),
            recovery_policy: PerceptionRecoveryPolicy::RecoverFromReceipt,
            capability_revision: format!("sha256:{}", "a".repeat(64)),
            input_digest: format!("sha256:{}", "b".repeat(64)),
            input_bytes: 10,
        })
        .await
        .unwrap();
    let media = artifacts
        .persist_exact(
            &invocation_id,
            PerceptionArtifactKind::SourceMedia,
            "audio/wav",
            b"RIFF-audio".to_vec(),
        )
        .await
        .unwrap();
    let revision = store
        .persist_perception_media(PerceptionMediaPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: 0,
            artifact_locator: media.locator,
        })
        .await
        .unwrap();
    store
        .advance_perception(PerceptionAdvance {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            expected_position: PerceptionExecutionPosition::MediaPersisted,
            next_position: PerceptionExecutionPosition::InferenceStarted,
        })
        .await
        .unwrap();
    let response = ModelResponse {
        id: "durable-receipt".into(),
        model: audio_model().reference,
        content: vec![ContentBlock::Text {
            text: "recovered words".into(),
        }],
        stop_reason: StopReason::EndTurn,
        usage: TokenUsage::default(),
    };
    artifacts
        .persist_exact(
            &invocation_id,
            PerceptionArtifactKind::ProviderReceipt,
            "application/json",
            serde_json::to_vec(&response).unwrap(),
        )
        .await
        .unwrap();
    let snapshot = store
        .perception_invocations(&session.id, "turn-receipt-recovery")
        .await
        .unwrap()
        .remove(0);

    let recovered = recover_perception_receipt(store.clone(), artifacts, snapshot)
        .await
        .unwrap();
    assert_eq!(recovered.text, "recovered words");
    assert_eq!(
        store
            .perception_invocations(&session.id, "turn-receipt-recovery")
            .await
            .unwrap()[0]
            .position,
        PerceptionExecutionPosition::ResultPersisted
    );
}

#[tokio::test]
async fn post_receipt_and_post_artifact_positions_resume_without_provider_replay() {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(
        SqliteSessionStore::open(directory.path().join("sessions.sqlite"))
            .await
            .unwrap(),
    );

    let session = running_perception_turn(&store, "turn-after-receipt").await;
    let artifacts = perception_artifacts(
        &directory.path().join("evidence.sqlite"),
        "turn-after-receipt",
    )
    .await;
    let invocation_id = PerceptionInvocationId::new();
    prepare_completed_inference(
        &store,
        &artifacts,
        &session,
        "turn-after-receipt",
        &invocation_id,
    )
    .await;
    let snapshot = store
        .perception_invocations(&session.id, "turn-after-receipt")
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        snapshot.position,
        PerceptionExecutionPosition::InferenceCompleted
    );
    let recovered = recover_perception_receipt(store.clone(), artifacts.clone(), snapshot)
        .await
        .unwrap();
    assert_eq!(recovered.text, "durable words");

    let second = tempfile::tempdir().unwrap();
    let second_store = Arc::new(
        SqliteSessionStore::open(second.path().join("sessions.sqlite"))
            .await
            .unwrap(),
    );
    let session = running_perception_turn(&second_store, "turn-after-artifact").await;
    let second_artifacts = perception_artifacts(
        &second.path().join("evidence.sqlite"),
        "turn-after-artifact",
    )
    .await;
    let invocation_id = PerceptionInvocationId::new();
    let (revision, response) = prepare_completed_inference(
        &second_store,
        &second_artifacts,
        &session,
        "turn-after-artifact",
        &invocation_id,
    )
    .await;
    let normalized = serde_json::json!({
        "schema_version": 1,
        "invocation_id": invocation_id.as_str(),
        "provider_response_id": response.id,
        "text": "durable words"
    });
    let output = second_artifacts
        .persist_exact(
            &invocation_id,
            PerceptionArtifactKind::NormalizedOutput,
            "application/json",
            serde_json::to_vec(&normalized).unwrap(),
        )
        .await
        .unwrap();
    second_store
        .persist_perception_artifact(PerceptionArtifactPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            artifact_locator: output.locator,
            output_digest: output.digest,
        })
        .await
        .unwrap();
    let snapshot = second_store
        .perception_invocations(&session.id, "turn-after-artifact")
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        snapshot.position,
        PerceptionExecutionPosition::ArtifactPersisted
    );
    let recovered = recover_perception_receipt(second_store.clone(), second_artifacts, snapshot)
        .await
        .unwrap();
    assert_eq!(
        recovered.provider_response_id,
        format!("receipt-{}", "turn-after-artifact")
    );
    assert_eq!(
        second_store
            .perception_invocations(&session.id, "turn-after-artifact")
            .await
            .unwrap()[0]
            .position,
        PerceptionExecutionPosition::ResultPersisted
    );
}

#[tokio::test]
async fn save_is_upsert() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();
    // Save again with a new name — should update, not duplicate.
    let mut updated = make_session("s1", SessionLifetime::Persistent);
    updated.name = "renamed".into();
    store.save(&updated).await.unwrap();

    let all = store
        .list(
            &ctx(),
            SessionFilter {
                include_archived: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "renamed");
}

#[tokio::test]
async fn delete_removes() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Ephemeral))
        .await
        .unwrap();
    store.delete(&SessionId::new("s1")).await.unwrap();
    assert!(store.get(&SessionId::new("s1")).await.unwrap().is_none());
}

#[tokio::test]
async fn archive_soft_deletes() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();
    store.archive(&SessionId::new("s1")).await.unwrap();

    // get returns None (treats archived as gone from active set)
    assert!(store.get(&SessionId::new("s1")).await.unwrap().is_none());

    // list with include_archived=false (default) hides archived
    let visible = store.list(&ctx(), SessionFilter::default()).await.unwrap();
    assert!(visible.iter().all(|s| s.id != SessionId::new("s1")));

    // list with include_archived=true brings it back
    let filter = SessionFilter {
        include_archived: true,
        ..Default::default()
    };
    let all = store.list(&ctx(), filter).await.unwrap();
    assert_eq!(all.len(), 1);
    assert!(all[0].archived);
    let persistent = store.list_persistent(true).await.unwrap();
    assert_eq!(persistent.len(), 1);
    assert!(persistent[0].archived);
}

#[tokio::test]
async fn archived_session_can_be_restored_with_history_intact() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let id = SessionId::new("s1");
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();
    store.archive(&id).await.unwrap();
    store.restore(&id).await.unwrap();
    assert_eq!(store.get(&id).await.unwrap().unwrap().id, id);
}

#[tokio::test]
async fn usage_accumulates_atomically_per_session() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let id = SessionId::new("s1");
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();
    store
        .record_usage(&id, 100, 20, Some(30_000))
        .await
        .unwrap();
    let usage = store.record_usage(&id, 50, 10, Some(15_000)).await.unwrap();
    assert_eq!(
        usage,
        SessionUsage {
            iterations: 2,
            input_tokens: 150,
            output_tokens: 30,
            cost_nano_usd: Some(45_000),
        }
    );
    assert_eq!(store.usage(&id).await.unwrap(), usage);
}

#[tokio::test]
async fn any_unpriced_iteration_makes_cumulative_cost_unknown() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let id = SessionId::new("s1");
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();
    store.record_usage(&id, 10, 2, Some(1_000)).await.unwrap();
    let usage = store.record_usage(&id, 5, 1, None).await.unwrap();
    assert_eq!(usage.cost_nano_usd, None);
}

#[tokio::test]
async fn usage_rejects_cost_beyond_sqlite_integer_range() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let id = SessionId::new("s1");
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();

    let error = store
        .record_usage(&id, 1, 1, Some(u64::MAX))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("SQLite INTEGER range"));
    assert_eq!(store.usage(&id).await.unwrap(), SessionUsage::default());
}

#[test]
fn legacy_usage_table_is_rejected_instead_of_migrated() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            "CREATE TABLE session_usage (session_id TEXT PRIMARY KEY, iterations INTEGER NOT NULL DEFAULT 0, input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0); INSERT INTO session_usage VALUES ('old', 1, 10, 2);",
        )
        .unwrap();
    assert!(matches!(
        SqliteSessionStore::init_schema(&conn),
        Err(SessionStoreError::IncompatibleSchema)
    ));
}

#[test]
fn current_schema_reopens_but_damaged_and_future_schemas_fail_closed() {
    let current = Connection::open_in_memory().unwrap();
    SqliteSessionStore::init_schema(&current).unwrap();
    SqliteSessionStore::init_schema(&current).unwrap();

    current
        .execute_batch("DROP INDEX idx_messages_user")
        .unwrap();
    assert!(matches!(
        SqliteSessionStore::init_schema(&current),
        Err(SessionStoreError::IncompatibleSchema)
    ));

    let future = Connection::open_in_memory().unwrap();
    future.execute_batch(SCHEMA_SQL).unwrap();
    future
        .pragma_update(None, "user_version", SESSION_SCHEMA_VERSION + 1)
        .unwrap();
    assert!(matches!(
        SqliteSessionStore::init_schema(&future),
        Err(SessionStoreError::IncompatibleSchema)
    ));
}

#[tokio::test]
async fn shared_open_accepts_only_the_declared_foreign_namespace() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("shared.db");
    drop(SqliteSessionStore::open(&path).await.unwrap());
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE registry_probe(value TEXT);")
        .unwrap();
    drop(connection);

    drop(
        SqliteSessionStore::open_shared(&path, &["registry_probe"])
            .await
            .unwrap(),
    );
    assert!(matches!(
        SqliteSessionStore::open(&path).await,
        Err(SessionStoreError::IncompatibleSchema)
    ));

    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE undeclared_probe(value TEXT);")
        .unwrap();
    drop(connection);
    assert!(matches!(
        SqliteSessionStore::open_shared(&path, &["registry_probe"]).await,
        Err(SessionStoreError::IncompatibleSchema)
    ));
}

#[tokio::test]
async fn list_filters_by_user() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();

    let mut s_a = make_session("s-a", SessionLifetime::Persistent);
    s_a.metadata.user_id = "alice".into();
    let mut s_b = make_session("s-b", SessionLifetime::Persistent);
    s_b.metadata.user_id = "bob".into();

    store.save(&s_a).await.unwrap();
    store.save(&s_b).await.unwrap();

    let filter = SessionFilter {
        identity: Some(sylvander_api::Identity {
            user_id: sylvander_api::UserId::new("alice"),
            agent_id: sylvander_api::AgentId::new("agent-1"),
            agent_instance_id: None,
            session_id: sylvander_api::SessionId::new("dummy"),
        }),
        ..Default::default()
    };
    let alice_sessions = store.list(&ctx(), filter).await.unwrap();
    assert_eq!(alice_sessions.len(), 1);
    assert_eq!(alice_sessions[0].id, SessionId::new("s-a"));
}

#[tokio::test]
async fn search_finds_by_name_substring() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut s1 = make_session("s1", SessionLifetime::Persistent);
    s1.name = "修复登录 bug".into();
    let mut s2 = make_session("s2", SessionLifetime::Persistent);
    s2.name = "重构 API".into();
    store.save(&s1).await.unwrap();
    store.save(&s2).await.unwrap();

    let hits = store.search(&ctx(), "登录", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, SessionId::new("s1"));
}

// ---- messages ----

#[tokio::test]
async fn append_message_assigns_seq() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();

    let m1 = store
        .append_message(
            &ctx(),
            &SessionId::new("s1"),
            MessageRole::User,
            serde_json::json!({"role":"user","content":"hi"}),
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let m2 = store
        .append_message(
            &ctx(),
            &SessionId::new("s1"),
            MessageRole::Assistant,
            serde_json::json!({"role":"assistant","content":[{"type":"text","text":"hello"}]}),
            Some("claude-sonnet-5"),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(m1.seq, 0);
    assert_eq!(m2.seq, 1);
    assert_eq!(m2.model_id.as_deref(), Some("claude-sonnet-5"));
}

#[tokio::test]
async fn read_history_returns_in_order() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();

    for i in 0..3 {
        store
            .append_message(
                &ctx(),
                &SessionId::new("s1"),
                MessageRole::User,
                serde_json::json!({"i": i}),
                None,
                None,
                None,
            )
            .await
            .unwrap();
    }

    let history = store
        .read_history(&ctx(), &SessionId::new("s1"), false, None)
        .await
        .unwrap();
    assert_eq!(history.len(), 3);
    for (i, m) in history.iter().enumerate() {
        assert_eq!(m.seq, i as u32);
    }
}

#[tokio::test]
async fn histories_and_compaction_are_isolated_by_agent_instance() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut session = make_session("sess-1", SessionLifetime::Persistent);
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    let now = session.created_at;
    let moderator_id = AgentInstanceId::new("moderator-instance");
    let worker_id = AgentInstanceId::new("worker-instance");
    let instance = |instance_id: AgentInstanceId, role: SessionAgentRole, ordinal| AgentInstance {
        instance_id,
        session_id: session.id.clone(),
        definition: AgentDefinitionKey {
            agent_id: AgentId::new("agent-1"),
            revision: 7,
        },
        origin: AgentInstanceOrigin::Defined,
        role,
        history_view: HistoryView::ForkSnapshot {
            base_sequence: 0,
            branch_id: format!("branch-{ordinal}"),
        },
        approval_route: ApprovalRoute::User,
        state: AgentInstanceState::Ready,
        lifecycle_revision: ordinal,
        capability_revision: format!("capability-{ordinal}"),
        created_at: now,
        updated_at: now,
    };
    let membership = SessionMembership::new(
        session.id.clone(),
        vec![
            instance(moderator_id.clone(), SessionAgentRole::Moderator, 0),
            instance(worker_id.clone(), SessionAgentRole::Worker, 1),
        ],
        SessionGovernance {
            session_id: session.id.clone(),
            moderator_instance_id: moderator_id.clone(),
            governance_revision: "governance-v1".into(),
            membership_revision: 0,
            lease_epoch: 1,
            fencing_token: 1,
            updated_at: now,
        },
    )
    .unwrap();
    store
        .save_session_membership(&membership, None)
        .await
        .unwrap();
    store
        .save_agent_instance_config(
            &AgentInstanceConfig {
                session_id: session.id.clone(),
                instance_id: worker_id.clone(),
                config_revision: 0,
                effective: effective_config(),
                updated_at: now,
            },
            None,
        )
        .await
        .unwrap();

    let moderator = ctx().with_agent_instance(moderator_id);
    let worker = ctx().with_agent_instance(worker_id);
    for (context, content) in [(&moderator, "moderator"), (&worker, "worker")] {
        store
            .append_message(
                context,
                &session.id,
                MessageRole::User,
                serde_json::json!({"content": content}),
                None,
                None,
                None,
            )
            .await
            .unwrap();
    }

    let moderator_history = store
        .read_history(&moderator, &session.id, false, None)
        .await
        .unwrap();
    let worker_history = store
        .read_history(&worker, &session.id, false, None)
        .await
        .unwrap();
    assert_eq!(moderator_history.len(), 1);
    assert_eq!(moderator_history[0].content["content"], "moderator");
    assert_eq!(worker_history.len(), 1);
    assert_eq!(worker_history[0].content["content"], "worker");

    store
        .mark_summarized(&moderator, &session.id, 0..2)
        .await
        .unwrap();
    assert_eq!(
        store
            .count_active_messages(&moderator, &session.id)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .count_active_messages(&worker, &session.id)
            .await
            .unwrap(),
        1
    );

    for (context, instance_id, turn_id) in [
        (&moderator, "moderator-instance", "moderator-turn"),
        (&worker, "worker-instance", "worker-turn"),
    ] {
        store
            .begin_turn(
                context,
                TurnStart {
                    session_id: session.id.clone(),
                    turn_id: turn_id.into(),
                    agent_instance_id: AgentInstanceId::new(instance_id),
                    config_revision: 0,
                    effective_config: effective_config(),
                    user_content: serde_json::json!({"content": turn_id}),
                    model_id: "model-a".into(),
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn read_history_excludes_summarized() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();
    for i in 0..3 {
        store
            .append_message(
                &ctx(),
                &SessionId::new("s1"),
                MessageRole::User,
                serde_json::json!({"i": i}),
                None,
                None,
                None,
            )
            .await
            .unwrap();
    }
    // Mark seq 0..2 (i.e. seq 0 and 1) as summarized.
    store
        .mark_summarized(&ctx(), &SessionId::new("s1"), 0..2)
        .await
        .unwrap();

    let active = store
        .read_history(&ctx(), &SessionId::new("s1"), false, None)
        .await
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].seq, 2);

    let all = store
        .read_history(&ctx(), &SessionId::new("s1"), true, None)
        .await
        .unwrap();
    assert_eq!(all.len(), 3);
}

#[tokio::test]
async fn count_active_messages() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();
    for _ in 0..5 {
        store
            .append_message(
                &ctx(),
                &SessionId::new("s1"),
                MessageRole::User,
                serde_json::json!({}),
                None,
                None,
                None,
            )
            .await
            .unwrap();
    }
    store
        .mark_summarized(&ctx(), &SessionId::new("s1"), 0..3)
        .await
        .unwrap();

    assert_eq!(
        store
            .count_active_messages(&ctx(), &SessionId::new("s1"))
            .await
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn cascade_delete_drops_messages() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();
    store
        .append_message(
            &ctx(),
            &SessionId::new("s1"),
            MessageRole::User,
            serde_json::json!({}),
            None,
            None,
            None,
        )
        .await
        .unwrap();

    store.delete(&SessionId::new("s1")).await.unwrap();

    // The message row is gone (CASCADE).
    let history = store
        .read_history(&ctx(), &SessionId::new("s1"), true, None)
        .await
        .unwrap();
    assert!(history.is_empty());
}

#[tokio::test]
async fn append_to_missing_session_errors() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let result = store
        .append_message(
            &ctx(),
            &SessionId::new("nonexistent"),
            MessageRole::User,
            serde_json::json!({}),
            None,
            None,
            None,
        )
        .await;
    assert!(matches!(result, Err(SessionStoreError::NotFound(_))));
}

#[tokio::test]
async fn concurrent_saves_serialize_safely() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store
        .save(&make_session("s1", SessionLifetime::Persistent))
        .await
        .unwrap();

    // Spawn 10 concurrent appends — must not deadlock or panic.
    let mut handles = Vec::new();
    for i in 0..10 {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            s.append_message(
                &ctx(),
                &SessionId::new("s1"),
                MessageRole::User,
                serde_json::json!({"i": i}),
                None,
                None,
                None,
            )
            .await
        }));
    }
    for h in handles {
        h.await.unwrap().unwrap();
    }

    let count = store
        .count_active_messages(&ctx(), &SessionId::new("s1"))
        .await
        .unwrap();
    assert_eq!(count, 10);

    // All seq values must be unique and contiguous.
    let history = store
        .read_history(&ctx(), &SessionId::new("s1"), false, None)
        .await
        .unwrap();
    let seqs: Vec<u32> = history.iter().map(|m| m.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable();
    assert_eq!(seqs, sorted, "seqs must be assigned uniquely");
}

#[tokio::test]
async fn file_backed_store_persists_across_opens() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("sessions.db");

    // Write one session and message.
    let s1 = SqliteSessionStore::open(&path).await.unwrap();
    s1.save(&make_session("p1", SessionLifetime::Persistent))
        .await
        .unwrap();
    s1.append_message(
        &ctx(),
        &SessionId::new("p1"),
        MessageRole::User,
        serde_json::json!({"hello": "world"}),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    drop(s1);

    // Reopen — data should still be there.
    let s2 = SqliteSessionStore::open(&path).await.unwrap();
    let found = s2.get(&SessionId::new("p1")).await.unwrap();
    assert!(found.is_some());
    let history = s2
        .read_history(&ctx(), &SessionId::new("p1"), false, None)
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content["hello"], "world");
}

#[tokio::test]
async fn health_revalidates_the_live_schema() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    store.verify_health().await.unwrap();

    store
        .inner
        .conn
        .lock()
        .await
        .execute_batch("DROP INDEX idx_sessions_updated;")
        .unwrap();

    assert!(matches!(
        store.verify_health().await,
        Err(SessionStoreError::IncompatibleSchema)
    ));
}
