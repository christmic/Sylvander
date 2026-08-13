use super::*;
use std::path::PathBuf;
use sylvander_agent::workspace_journal::WorkspaceMutationJournal;

use crate::storage::session::{ModelRecoveryClassification, ModelRecoveryDecision};
use crate::storage::workspace_journal::WorkspaceJournal;

/// Default session context used by every test. Identity is the
/// stable "user-1" from `test_meta` so ownership assertions share one
/// authenticated subject.
fn ctx() -> sylvander_api::SessionContext {
    sylvander_api::SessionContext::new("user-1", "agent-1", "sess-1")
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

    let persistent = store.list_persistent().await.unwrap();
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
        config_revision: 1,
        effective_config: effective.clone(),
        user_content: serde_json::json!({"role": "user", "content": "hello"}),
        model_id: "model-a".into(),
    };
    let message = store.begin_turn(&ctx(), start.clone()).await.unwrap();
    assert_eq!(message.seq, 0);
    let snapshot = store.turn(&session.id, "turn-1").await.unwrap().unwrap();
    assert_eq!(snapshot.config_revision, 1);
    assert_eq!(snapshot.effective_config, effective);
    assert_eq!(snapshot.state, TurnState::Running);
    assert_eq!(snapshot.ended_at, None);

    let assistant = store
        .complete_turn(
            &ctx(),
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
                &ctx(),
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
    store.begin_turn(&ctx(), failed_start).await.unwrap();
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

    assert!(store.begin_turn(&ctx(), start).await.is_err());
    let stale = TurnStart {
        session_id: session.id.clone(),
        turn_id: "turn-stale".into(),
        config_revision: 0,
        effective_config: effective_config(),
        user_content: serde_json::json!({"role": "user", "content": "stale"}),
        model_id: "model-a".into(),
    };
    assert!(matches!(
        store.begin_turn(&ctx(), stale).await,
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
            .read_history(&ctx(), &session.id, false, None)
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
            &ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-model-ledger".into(),
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
            &ctx(),
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
            &ctx(),
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
    store
        .begin_turn(
            &ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-tools".into(),
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
                &ctx(),
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
    store
        .begin_turn(
            &ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-recovery".into(),
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
                &ctx(),
                TurnStart {
                    session_id: session.id.clone(),
                    turn_id: "turn-must-wait".into(),
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
    store
        .begin_turn(
            &ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-result".into(),
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
                &ctx().with_trace_id("turn-result"),
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
        .read_history(&ctx(), &session.id, false, None)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[1].role, MessageRole::Tool);
    assert_eq!(history[1].content, content);
    assert!(
        store
            .persist_tool_result(
                &ctx(),
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
    store
        .begin_turn(
            &ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-crashed".into(),
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
    store
        .finish_turn(
            &session.id,
            "turn-crashed",
            TurnState::Failed,
            Some(TurnFailureKind::Persistence),
        )
        .await
        .unwrap();

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
    let observed = observability.snapshot();
    assert_eq!(observed.tool_recoveries_classified, 1);
    assert_eq!(observed.tool_recoveries_manual, 1);
}

#[tokio::test]
async fn boot_recovery_reconciles_committed_workspace_effect_without_replay() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let mut session = make_session("sess-1", SessionLifetime::Persistent);
    session.effective_config = Some(effective_config());
    store.save(&session).await.unwrap();
    store
        .begin_turn(
            &ctx(),
            TurnStart {
                session_id: session.id.clone(),
                turn_id: "turn-write".into(),
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
        .mark_summarized(&SessionId::new("s1"), 0..2)
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
        .mark_summarized(&SessionId::new("s1"), 0..3)
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
