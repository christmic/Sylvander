use super::*;
use std::path::PathBuf;

/// Default session context used by every test. Identity is the
/// stable "user-1" from `test_meta` so ownership assertions share one
/// authenticated subject.
fn ctx() -> sylvander_protocol::SessionContext {
    sylvander_protocol::SessionContext::new("user-1", "agent-1", "sess-1")
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

fn effective_config() -> sylvander_protocol::SessionEffectiveConfig {
    let source = sylvander_protocol::SessionConfigSource {
        kind: sylvander_protocol::SessionConfigSourceKind::AgentDefault,
        reference: Some("assistant@7".into()),
    };
    sylvander_protocol::SessionEffectiveConfig {
        agent_id: AgentId::new("agent-1"),
        agent_revision: 7,
        provider_id: "primary".into(),
        provider_revision: 1,
        model_id: "model-a".into(),
        model_revision: 1,
        reasoning_effort: sylvander_protocol::ReasoningEffort::Medium,
        permissions: sylvander_protocol::PermissionProfile::default(),
        prompt_profile: Some("coding".into()),
        system_prompt_sha256: "abc123".into(),
        prompt_manifest: sylvander_protocol::PromptManifest {
            layers: Vec::new(),
            aggregate_sha256: "manifest".into(),
            total_bytes: 0,
        },
        agent_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: "/agent".into(),
            read_only: false,
            instruction_focus: None,
        }),
        user_workspace: Some(sylvander_protocol::SessionWorkspaceBinding {
            execution_target: "local".into(),
            path: "/project".into(),
            read_only: false,
            instruction_focus: None,
        }),
        workspace_mounts: Vec::new(),
        execution_target: "local".into(),
        provenance: sylvander_protocol::SessionConfigProvenance {
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
    session.config_overrides.model = Some(sylvander_protocol::ModelSelection {
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
        Some(sylvander_protocol::ModelSelection {
            provider_id: "provider-a".into(),
            model_id: "model-a".into(),
        })
    );
    assert_eq!(s.effective_config, session.effective_config);
}

#[tokio::test]
async fn opening_legacy_database_adds_session_config_columns() {
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

    let store = SqliteSessionStore::open(&path).await.unwrap();
    let session = make_session("migrated", SessionLifetime::Persistent);
    store.save(&session).await.unwrap();
    let loaded = store.get(&session.id).await.unwrap().unwrap();

    assert_eq!(loaded.config_revision, 0);
    assert_eq!(
        loaded.config_overrides,
        sylvander_protocol::SessionConfigOverrides::default()
    );
    assert!(loaded.effective_config.is_none());
}

#[tokio::test]
async fn config_updates_are_optimistic_and_turn_start_is_atomic() {
    let store = SqliteSessionStore::open_in_memory().await.unwrap();
    let session = make_session("s1", SessionLifetime::Persistent);
    store.save(&session).await.unwrap();
    let effective = effective_config();
    let overrides = sylvander_protocol::SessionConfigOverrides {
        model: Some(sylvander_protocol::ModelSelection {
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
    let snapshot = store
        .turn_config(&session.id, "turn-1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.config_revision, 1);
    assert_eq!(snapshot.effective_config, effective);

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
            .turn_config(&session.id, "turn-stale")
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
        1
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
    let overrides = sylvander_protocol::SessionConfigOverrides {
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
fn legacy_usage_table_migrates_with_unknown_historical_cost() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
            "CREATE TABLE session_usage (session_id TEXT PRIMARY KEY, iterations INTEGER NOT NULL DEFAULT 0, input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0); INSERT INTO session_usage VALUES ('old', 1, 10, 2);",
        )
        .unwrap();
    SqliteSessionStore::init_schema(&conn).unwrap();
    assert_eq!(
        read_usage(&conn, &SessionId::new("old"))
            .unwrap()
            .cost_nano_usd,
        None
    );
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
        identity: Some(sylvander_protocol::Identity {
            user_id: sylvander_protocol::types::UserId::new("alice"),
            agent_id: sylvander_protocol::types::AgentId::new("agent-1"),
            session_id: sylvander_protocol::types::SessionId::new("dummy"),
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
