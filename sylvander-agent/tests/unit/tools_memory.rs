use super::*;

impl MemoryExecutionContext {
    pub(crate) fn privileged_for_test(actor: MemoryActorKind) -> Self {
        Self {
            authority: MemoryAuthority::ApplicationIssued,
            actor,
            user_id: Some(UserId::new("alice")),
            agent_id: Some(AgentId::new("agent-a")),
            session_id: Some(SessionId::new("session")),
            authorized_workspace_ids: Vec::new(),
            trace_id: None,
        }
    }
}

fn session(user: &str, agent: &str, session: &str) -> SessionContext {
    SessionContext::new(user, agent, session)
}

fn worker(session: &SessionContext) -> MemoryExecutionContext {
    MemoryExecutionContext::application_worker(session)
}

fn privileged(actor: MemoryActorKind) -> MemoryExecutionContext {
    MemoryExecutionContext {
        authority: MemoryAuthority::ApplicationIssued,
        actor,
        user_id: Some(UserId::new("alice")),
        agent_id: Some(AgentId::new("a1")),
        session_id: Some(SessionId::new("s1")),
        authorized_workspace_ids: Vec::new(),
        trace_id: None,
    }
}

#[tokio::test]
async fn relationship_append_search_and_filters() {
    let store = InMemoryMemoryStore::new();
    let alice = session("alice", "a1", "s1");
    let ctx = worker(&alice);
    let preference = store
        .append_relationship(
            &ctx,
            MemoryAppend::new("The user prefers Rust")
                .with_kind(MemoryKind::Preference)
                .with_tag("language")
                .with_importance(Importance::High),
        )
        .await
        .unwrap();
    assert_eq!(preference.revision, 1);
    assert_eq!(preference.provenance.actor, MemoryActorKind::Worker);
    assert_eq!(
        preference.provenance.source,
        MemoryProvenanceSource::Runtime
    );
    assert!(preference.provenance.trusted);
    store
        .append_relationship(
            &ctx,
            MemoryAppend::new("we chose tokio").with_kind(MemoryKind::Decision),
        )
        .await
        .unwrap();

    let results = store
        .search_relationship(
            &ctx,
            "RUST",
            MemoryFilter {
                kind: Some(MemoryKind::Preference),
                min_importance: Some(Importance::High),
                limit: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(results, [preference]);
}

#[tokio::test]
async fn relationship_operations_isolate_user_and_agent() {
    let store = InMemoryMemoryStore::new();
    let alice = worker(&session("alice", "a1", "s1"));
    let bob = worker(&session("bob", "a1", "s2"));
    let other_agent = worker(&session("alice", "a2", "s3"));
    let entry = store
        .append_relationship(&alice, MemoryAppend::new("alice secret"))
        .await
        .unwrap();

    for outsider in [&bob, &other_agent] {
        assert!(
            store
                .search_relationship(outsider, "", MemoryFilter::default())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .get_relationship(outsider, &entry.id)
                .await
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        store
            .get_relationship(&alice, &entry.id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "alice secret"
    );
}

#[tokio::test]
async fn foreign_and_missing_deletes_are_indistinguishable() {
    let store = InMemoryMemoryStore::new();
    let alice = worker(&session("alice", "a1", "s1"));
    let bob = worker(&session("bob", "a1", "s2"));
    let entry = store
        .append_relationship(&alice, MemoryAppend::new("keep"))
        .await
        .unwrap();

    let foreign = store
        .delete_relationship(&bob, &entry.id, entry.revision)
        .await
        .unwrap_err();
    let missing = store
        .delete_relationship(&bob, "00000000-0000-0000-0000-000000000000", entry.revision)
        .await
        .unwrap_err();
    assert_eq!(foreign.to_string(), missing.to_string());
    assert!(
        store
            .get_relationship(&alice, &entry.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn guardian_and_system_fail_closed_for_relationship_operations() {
    let store = InMemoryMemoryStore::new();
    for ctx in [
        privileged(MemoryActorKind::Guardian),
        privileged(MemoryActorKind::SystemService),
    ] {
        assert!(matches!(
            store
                .append_relationship(&ctx, MemoryAppend::new("forbidden"))
                .await,
            Err(MemoryStoreError::AccessDenied)
        ));
        assert!(matches!(
            store
                .search_relationship(&ctx, "", MemoryFilter::default())
                .await,
            Err(MemoryStoreError::AccessDenied)
        ));
        assert!(matches!(
            store.get_relationship(&ctx, "valid-id").await,
            Err(MemoryStoreError::AccessDenied)
        ));
        assert!(matches!(
            store
                .update_relationship(&ctx, "valid-id", 1, MemoryPatch::default())
                .await,
            Err(MemoryStoreError::AccessDenied)
        ));
        assert!(matches!(
            store
                .supersede_relationship(&ctx, "valid-id", 1, MemoryAppend::new("forbidden"))
                .await,
            Err(MemoryStoreError::AccessDenied)
        ));
        assert!(matches!(
            store.delete_relationship(&ctx, "valid-id", 1).await,
            Err(MemoryStoreError::AccessDenied)
        ));
    }
}

#[tokio::test]
async fn incomplete_worker_context_fails_closed() {
    let store = InMemoryMemoryStore::new();
    let ctx = MemoryExecutionContext {
        authority: MemoryAuthority::ApplicationIssued,
        actor: MemoryActorKind::Worker,
        user_id: Some(UserId::new("alice")),
        agent_id: Some(AgentId::new("a1")),
        session_id: None,
        authorized_workspace_ids: Vec::new(),
        trace_id: None,
    };
    assert!(matches!(
        store
            .append_relationship(&ctx, MemoryAppend::new("forbidden"))
            .await,
        Err(MemoryStoreError::AccessDenied)
    ));
}

#[tokio::test]
async fn public_memory_bounds_fail_closed() {
    let store = InMemoryMemoryStore::new();
    let ctx = worker(&session("alice", "a1", "s1"));
    assert!(matches!(
        store
            .append_relationship(
                &ctx,
                MemoryAppend::new("x".repeat(MAX_MEMORY_CONTENT_BYTES + 1))
            )
            .await,
        Err(MemoryStoreError::InvalidInput)
    ));
    for ttl in [0, MAX_MEMORY_TTL_SECONDS + 1] {
        assert!(matches!(
            store
                .append_relationship(&ctx, MemoryAppend::new("ttl").with_ttl(ttl))
                .await,
            Err(MemoryStoreError::InvalidInput)
        ));
    }
    for key in [
        "provenance",
        "owner",
        "scope",
        "revision",
        "actor",
        "user_id",
        "agent_id",
        "session_id",
        "trace_id",
        "SYLVANDER.audit",
    ] {
        let mut append = MemoryAppend::new("forged metadata");
        append.metadata.insert(key.into(), "attacker".into());
        assert!(matches!(
            store.append_relationship(&ctx, append).await,
            Err(MemoryStoreError::InvalidInput)
        ));
    }
    assert!(matches!(
        store
            .search_relationship(
                &ctx,
                &"q".repeat(MAX_MEMORY_QUERY_BYTES + 1),
                MemoryFilter::default()
            )
            .await,
        Err(MemoryStoreError::InvalidInput)
    ));
    assert!(matches!(
        store
            .search_relationship(
                &ctx,
                "",
                MemoryFilter {
                    limit: Some(MAX_MEMORY_RESULTS + 1),
                    ..MemoryFilter::default()
                }
            )
            .await,
        Err(MemoryStoreError::InvalidInput)
    ));
}

#[tokio::test]
async fn delete_owned_entry() {
    let store = InMemoryMemoryStore::new();
    let ctx = worker(&session("alice", "a1", "s1"));
    let entry = store
        .append_relationship(&ctx, MemoryAppend::new("drop"))
        .await
        .unwrap();
    assert!(matches!(
        store
            .delete_relationship(&ctx, &entry.id, entry.revision + 1)
            .await,
        Err(MemoryStoreError::Conflict)
    ));
    store
        .delete_relationship(&ctx, &entry.id, entry.revision)
        .await
        .unwrap();
    assert!(
        store
            .get_relationship(&ctx, &entry.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn delete_restricts_live_supersession_references() {
    let store = InMemoryMemoryStore::new();
    let ctx = worker(&session("alice", "a1", "s1"));
    let original = store
        .append_relationship(&ctx, MemoryAppend::new("old"))
        .await
        .unwrap();
    let replacement = store
        .supersede_relationship(
            &ctx,
            &original.id,
            original.revision,
            MemoryAppend::new("new"),
        )
        .await
        .unwrap();
    assert!(matches!(
        store
            .delete_relationship(&ctx, &replacement.id, replacement.revision)
            .await,
        Err(MemoryStoreError::Conflict)
    ));
}

#[tokio::test]
async fn only_expiry_patch_adopts_current_retention_policy_revision() {
    let policy = RelationshipMemoryRetentionPolicy::new(2, 2, 3, 1, 2, 10).unwrap();
    let store = InMemoryMemoryStore::with_retention_policy(policy);
    let ctx = worker(&session("alice", "a1", "s1"));
    let entry = store
        .append_relationship(&ctx, MemoryAppend::new("before"))
        .await
        .unwrap();
    store.entries.write().await[0].retention_policy_revision = 1;

    let content = store
        .update_relationship(
            &ctx,
            &entry.id,
            entry.revision,
            MemoryPatch {
                content: Some("after".into()),
                ..MemoryPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(content.retention_policy_revision, 1);
    let expiry = store
        .update_relationship(
            &ctx,
            &entry.id,
            content.revision,
            MemoryPatch {
                expiry: Some(MemoryExpiryPatch::AfterSeconds(60)),
                ..MemoryPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(expiry.retention_policy_revision, 2);
}

#[tokio::test]
async fn update_and_supersede_are_cas_guarded_and_hide_inactive() {
    let store = InMemoryMemoryStore::new();
    let ctx = worker(&session("alice", "a1", "s1"));
    let original = store
        .append_relationship(&ctx, MemoryAppend::new("old").with_ttl(60))
        .await
        .unwrap();
    let patch = MemoryPatch {
        content: Some("updated".into()),
        importance: Some(Importance::Critical),
        expiry: Some(MemoryExpiryPatch::AfterSeconds(30)),
        ..MemoryPatch::default()
    };
    assert!(matches!(
        store
            .update_relationship(&ctx, &original.id, 2, patch.clone())
            .await,
        Err(MemoryStoreError::Conflict)
    ));
    let updated = store
        .update_relationship(&ctx, &original.id, 1, patch)
        .await
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.content, "updated");
    assert_eq!(updated.importance, Importance::Critical);
    assert!(updated.expires_at.is_some());
    assert_eq!(updated.provenance, original.provenance);

    let replacement = store
        .supersede_relationship(
            &ctx,
            &original.id,
            updated.revision,
            MemoryAppend::new("replacement"),
        )
        .await
        .unwrap();
    assert_eq!(replacement.revision, 1);
    assert!(
        store
            .get_relationship(&ctx, &original.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .search_relationship(&ctx, "", MemoryFilter::default())
            .await
            .unwrap(),
        [replacement]
    );
    assert!(matches!(
        store.delete_relationship(&ctx, &original.id, 3).await,
        Err(MemoryStoreError::NotFound)
    ));
}

#[test]
fn append_builders_preserve_caller_fields_only() {
    let append = MemoryAppend::new("we chose Rust")
        .with_kind(MemoryKind::Decision)
        .with_tag("architecture")
        .with_importance(Importance::High)
        .with_reference(MemoryReference::File {
            path: "/Cargo.toml".into(),
        });
    assert_eq!(append.kind, MemoryKind::Decision);
    assert_eq!(append.importance, Importance::High);
    assert_eq!(append.references.len(), 1);
    assert_eq!(append.tags, ["architecture"]);
}

#[test]
fn application_context_hashes_untrusted_trace_identifiers() {
    let raw_trace = format!("private\n\0{}", "x".repeat(128 * 1024));
    let session = session("alice", "a1", "s1").with_trace_id(&raw_trace);
    let worker = MemoryExecutionContext::application_worker(&session);
    assert_eq!(worker.actor(), MemoryActorKind::Worker);
    assert_eq!(worker.user_id(), Some(&UserId::new("alice")));
    assert_eq!(worker.agent_id(), Some(&AgentId::new("a1")));
    assert_eq!(worker.session_id(), Some(&SessionId::new("s1")));
    let trace = worker.trace_id().unwrap();
    assert_eq!(trace, memory_trace_digest(&raw_trace));
    assert_eq!(trace.len(), 71);
    assert!(trace.starts_with("sha256:"));
    assert!(trace[7..].bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(!trace.contains("private"));
    assert!(!trace.chars().any(char::is_control));
    assert!(worker.authorized_workspace_ids().is_empty());
    assert_eq!(
        worker.relationship_owner().unwrap(),
        MemoryOwner::Relationship {
            user_id: UserId::new("alice"),
            agent_id: AgentId::new("a1"),
        }
    );
}

#[tokio::test]
async fn retention_policy_applies_default_and_rejects_unbounded_lifetimes() {
    let policy = RelationshipMemoryRetentionPolicy::new(7, 2, 3, 1, 2, 10).unwrap();
    let store = InMemoryMemoryStore::with_retention_policy(policy);
    let ctx = worker(&session("alice", "a1", "s1"));
    let defaulted = store
        .append_relationship(&ctx, MemoryAppend::new("default"))
        .await
        .unwrap();
    assert_eq!(defaulted.retention_policy_revision, 7);
    assert_eq!(
        defaulted.expires_at.unwrap() - defaulted.created_at,
        2 * 24 * 60 * 60
    );
    assert!(
        store
            .append_relationship(&ctx, MemoryAppend::new("shorter").with_ttl(60))
            .await
            .is_ok()
    );
    assert!(matches!(
        store
            .append_relationship(
                &ctx,
                MemoryAppend::new("too long").with_ttl(4 * 24 * 60 * 60)
            )
            .await,
        Err(MemoryStoreError::InvalidInput)
    ));
    assert!(matches!(
        store
            .update_relationship(
                &ctx,
                &defaulted.id,
                defaulted.revision,
                MemoryPatch {
                    expiry: Some(MemoryExpiryPatch::Never),
                    ..MemoryPatch::default()
                },
            )
            .await,
        Err(MemoryStoreError::InvalidInput)
    ));
}
