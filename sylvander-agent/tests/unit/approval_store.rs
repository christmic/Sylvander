use super::*;

fn digest(character: char) -> String {
    format!("sha256:{}", character.to_string().repeat(64))
}

fn context(user: &str, agent: &str, policy: char, capability: char) -> ApprovalGrantContext {
    ApprovalGrantContext::new(
        user,
        AgentId::new(agent),
        digest(policy),
        digest(capability),
    )
}

fn request(operation: &str, input: serde_json::Value) -> ToolUseRequest {
    ToolUseRequest {
        call_id: "call".into(),
        tool_name: operation.into(),
        input,
    }
}

#[test]
fn grant_key_is_stable_across_json_object_order() {
    let scope = context("user-1", "agent-1", '1', '2');
    let first = scope.key_for(&request(
        "write",
        serde_json::json!({"content": "x", "file_path": "a.rs"}),
    ));
    let second = scope.key_for(&request(
        "write",
        serde_json::json!({"file_path": "a.rs", "content": "x"}),
    ));
    assert_eq!(first, second);
}

#[test]
fn every_authorization_dimension_invalidates_a_grant() {
    let input = serde_json::json!({"file_path": "a.rs"});
    let original = context("user-1", "agent-1", '1', '2').key_for(&request("write", input.clone()));
    let variants = [
        context("user-2", "agent-1", '1', '2').key_for(&request("write", input.clone())),
        context("user-1", "agent-2", '1', '2').key_for(&request("write", input.clone())),
        context("user-1", "agent-1", '3', '2').key_for(&request("write", input.clone())),
        context("user-1", "agent-1", '1', '4').key_for(&request("write", input.clone())),
        context("user-1", "agent-1", '1', '2').key_for(&request("edit", input.clone())),
        context("user-1", "agent-1", '1', '2')
            .key_for(&request("write", serde_json::json!({"file_path": "b.rs"}))),
    ];
    assert!(variants.iter().all(|variant| variant != &original));
}

#[tokio::test]
async fn session_and_persistent_grants_have_exact_lifetimes() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("approvals.json");
    let first = SessionId::new("session-1");
    let second = SessionId::new("session-2");
    let session_grant =
        context("user", "agent", '1', '2').key_for(&request("write", serde_json::json!({})));
    let persistent_grant =
        context("user", "agent", '1', '2').key_for(&request("read", serde_json::json!({})));

    let mut memory = ApprovalMemory::load(Some(path.clone())).expect("load");
    assert_eq!(
        memory.allowed_scopes(true),
        vec![
            sylvander_protocol::ApprovalScope::Once,
            sylvander_protocol::ApprovalScope::Session,
            sylvander_protocol::ApprovalScope::Persistent,
        ]
    );
    memory
        .remember(
            &first,
            session_grant.clone(),
            sylvander_protocol::ApprovalScope::Session,
            true,
        )
        .await
        .expect("session grant");
    memory
        .remember(
            &first,
            persistent_grant.clone(),
            sylvander_protocol::ApprovalScope::Persistent,
            true,
        )
        .await
        .expect("persistent grant");
    assert!(memory.contains(&first, &session_grant).await);
    assert!(!memory.contains(&second, &session_grant).await);

    let reloaded = ApprovalMemory::load(Some(path)).expect("reload");
    assert!(reloaded.contains(&second, &persistent_grant).await);
    assert!(!reloaded.contains(&first, &session_grant).await);
}

#[tokio::test]
async fn agent_runs_share_one_process_safe_persistent_store() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("approvals.json");
    let mut first = ApprovalMemory::load(Some(path.clone())).expect("first");
    let mut second = ApprovalMemory::load(Some(path.clone())).expect("second");
    let session = SessionId::new("session");
    let first_grant =
        context("user", "agent-a", '1', '2').key_for(&request("read", serde_json::json!({})));
    let second_grant =
        context("user", "agent-b", '1', '2').key_for(&request("write", serde_json::json!({})));

    first
        .remember(
            &session,
            first_grant.clone(),
            sylvander_protocol::ApprovalScope::Persistent,
            true,
        )
        .await
        .expect("first grant");
    assert!(second.contains(&session, &first_grant).await);
    second
        .remember(
            &session,
            second_grant.clone(),
            sylvander_protocol::ApprovalScope::Persistent,
            true,
        )
        .await
        .expect("second grant");

    drop(first);
    drop(second);
    let reloaded = ApprovalMemory::load(Some(path)).expect("reload");
    assert!(reloaded.contains(&session, &first_grant).await);
    assert!(reloaded.contains(&session, &second_grant).await);
}

#[tokio::test]
async fn persistent_scope_requires_authenticated_stable_identity() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut memory =
        ApprovalMemory::load(Some(directory.path().join("approvals.json"))).expect("load");
    let grant =
        context("user", "agent", '1', '2').key_for(&request("write", serde_json::json!({})));

    assert_eq!(
        memory.allowed_scopes(false),
        vec![
            sylvander_protocol::ApprovalScope::Once,
            sylvander_protocol::ApprovalScope::Session,
        ]
    );
    let error = memory
        .remember(
            &SessionId::new("session"),
            grant,
            sylvander_protocol::ApprovalScope::Persistent,
            false,
        )
        .await
        .expect_err("unauthenticated identity must fail");
    assert!(error.contains("Runtime-authenticated stable identity"));
}

#[tokio::test]
async fn store_contains_hashes_but_not_raw_resource_arguments() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("approvals.json");
    let mut memory = ApprovalMemory::load(Some(path.clone())).expect("load");
    let grant = context("user", "agent", '1', '2').key_for(&request(
        "write",
        serde_json::json!({"file_path": "private/roadmap.md", "content": "do not persist"}),
    ));
    memory
        .remember(
            &SessionId::new("session"),
            grant,
            sylvander_protocol::ApprovalScope::Persistent,
            true,
        )
        .await
        .expect("persist");

    let encoded = std::fs::read_to_string(path).expect("read");
    assert!(!encoded.contains("private/roadmap.md"));
    assert!(!encoded.contains("do not persist"));
    assert!(encoded.contains("\"schema_version\": 1"));
}

#[test]
fn latest_schema_rejects_legacy_and_unknown_files() {
    let directory = tempfile::tempdir().expect("tempdir");
    let legacy = directory.path().join("legacy.json");
    std::fs::write(&legacy, r#"{"fingerprints":["write:{}"]}"#).expect("write");
    assert!(ApprovalMemory::load(Some(legacy)).is_err());

    let future = directory.path().join("future.json");
    std::fs::write(&future, r#"{"schema_version":2,"grants":[]}"#).expect("write");
    assert!(ApprovalMemory::load(Some(future)).is_err());
}

#[test]
fn approval_policy_revision_tracks_permissions_and_rule_order() {
    let ask = sylvander_protocol::PermissionProfile {
        file_access: sylvander_protocol::FileAccess::WorkspaceWrite,
        network_access: sylvander_protocol::NetworkAccess::Denied,
        approval_policy: sylvander_protocol::ApprovalPolicy::Ask,
    };
    let deny = sylvander_protocol::PermissionProfile {
        approval_policy: sylvander_protocol::ApprovalPolicy::Deny,
        ..ask.clone()
    };
    let approve_rule = ApprovalRule {
        tools: vec!["read".into()],
        action: RuleAction::AutoApprove,
    };
    let reject_rule = ApprovalRule {
        tools: vec!["write".into()],
        action: RuleAction::AutoReject {
            reason: "policy".into(),
        },
    };

    assert_ne!(
        approval_policy_revision(&ask, &[]),
        approval_policy_revision(&deny, &[])
    );
    assert_ne!(
        approval_policy_revision(&ask, &[approve_rule.clone(), reject_rule.clone()]),
        approval_policy_revision(&ask, &[reject_rule, approve_rule])
    );
}
