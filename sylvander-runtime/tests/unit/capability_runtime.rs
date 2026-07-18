use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use sylvander_protocol::{AgentId, SessionContext};

use super::*;

type RecordedCall = (CapabilityActor, RuntimeOwnerScope, Value);
type RecordedCalls = Arc<Mutex<Vec<RecordedCall>>>;

#[derive(Clone)]
struct RecordingCapability {
    definition: CapabilityDefinition,
    calls: RecordedCalls,
}

impl RecordingCapability {
    fn new(name: &str, class: CapabilityClass, calls: RecordedCalls) -> Self {
        let schema = json!({"type": "object", "properties": {"value": {"type": "string"}}});
        Self {
            definition: CapabilityDefinition {
                name: name.into(),
                version: 1,
                class,
                schema_digest: value_digest(&schema),
                schema,
            },
            calls,
        }
    }
}

#[async_trait]
impl RuntimeCapability for RecordingCapability {
    fn definition(&self) -> CapabilityDefinition {
        self.definition.clone()
    }

    async fn invoke(
        &self,
        invocation: AuthorizedCapabilityInvocation<'_>,
    ) -> Result<Value, CapabilityRuntimeError> {
        assert!(!invocation.invocation_id.is_empty());
        self.calls.lock().unwrap().push((
            invocation.actor,
            invocation.owner.clone(),
            invocation.input.clone(),
        ));
        Ok(json!({"ok": true}))
    }
}

#[derive(Default)]
struct RecordingAudit {
    records: Mutex<Vec<CapabilityAuditRecord>>,
    failures: Mutex<VecDeque<bool>>,
}

impl CapabilityAuditSink for RecordingAudit {
    fn record(&self, record: &CapabilityAuditRecord) -> Result<(), ()> {
        if self.failures.lock().unwrap().pop_front().unwrap_or(false) {
            return Err(());
        }
        self.records.lock().unwrap().push(record.clone());
        Ok(())
    }
}

fn runtime(
    audit: Arc<RecordingAudit>,
) -> (
    ActorCapabilityRuntime,
    GuardianServiceIdentity,
    RecordedCalls,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worker = CapabilityRegistry::new()
        .register(RecordingCapability::new(
            "command",
            CapabilityClass::Terminal,
            calls.clone(),
        ))
        .unwrap()
        .register(RecordingCapability::new(
            "candidate_append",
            CapabilityClass::AgentCandidateAppend,
            calls.clone(),
        ))
        .unwrap();
    let guardian = CapabilityRegistry::new()
        .register(RecordingCapability::new(
            "canonical_commit",
            CapabilityClass::CanonicalMemoryMutation,
            calls.clone(),
        ))
        .unwrap();
    let identity = GuardianServiceIdentity::issue("guardian.curator", 7, 2_000).unwrap();
    (
        ActorCapabilityRuntime::new(worker, guardian, identity.clone(), 11, audit).unwrap(),
        identity,
        calls,
    )
}

#[tokio::test]
async fn worker_discovery_and_forged_guardian_route_do_not_reveal_schema() {
    let audit = Arc::new(RecordingAudit::default());
    let (runtime, _, calls) = runtime(audit);
    let snapshot = runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::from(["workspace-a".into()]),
    );

    let names = snapshot
        .definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["candidate_append", "command"]);
    assert_eq!(
        snapshot.invoke("canonical_commit", &json!({}), 1_000).await,
        Err(CapabilityRuntimeError::CapabilityUnavailable)
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invocation_rejects_nested_owner_selection_before_handler_or_audit() {
    let audit = Arc::new(RecordingAudit::default());
    let (runtime, _, calls) = runtime(audit.clone());
    let snapshot = runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::new(),
    );

    assert_eq!(
        snapshot
            .invoke(
                "candidate_append",
                &json!({"value": "ok", "metadata": {"user_id": "bob"}}),
                1_000,
            )
            .await,
        Err(CapabilityRuntimeError::AccessDenied)
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(audit.records.lock().unwrap().is_empty());
}

#[tokio::test]
async fn worker_handler_receives_only_runtime_derived_owner_and_terminal_audit() {
    let audit = Arc::new(RecordingAudit::default());
    let (runtime, _, calls) = runtime(audit.clone());
    let snapshot = runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::from(["workspace-a".into()]),
    );

    assert_eq!(
        snapshot
            .invoke("candidate_append", &json!({"value": "preference"}), 1_000)
            .await,
        Ok(json!({"ok": true}))
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, CapabilityActor::Worker);
    assert_eq!(calls[0].1.user_id.as_ref().unwrap().0, "alice");
    assert_eq!(calls[0].1.agent_id.0, "agent-a");
    assert_eq!(calls[0].1.session_id.as_ref().unwrap().0, "session-a");
    assert_eq!(
        calls[0].1.workspace_ids,
        BTreeSet::from(["workspace-a".into()])
    );
    drop(calls);

    let records = audit.records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].phase, CapabilityAuditPhase::Authorized);
    assert_eq!(records[1].phase, CapabilityAuditPhase::Completed);
    assert_eq!(records[1].outcome, CapabilityAuditOutcome::Succeeded);
    assert_eq!(records[0].invocation_id, records[1].invocation_id);
    assert!(!records[0].owner_digest.contains("alice"));
}

#[tokio::test]
async fn unavailable_pre_execution_audit_fails_closed() {
    let audit = Arc::new(RecordingAudit::default());
    audit.failures.lock().unwrap().push_back(true);
    let (runtime, _, calls) = runtime(audit);
    let snapshot = runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::new(),
    );

    assert_eq!(
        snapshot.invoke("command", &json!({}), 1_000).await,
        Err(CapabilityRuntimeError::AuditUnavailable)
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn terminal_audit_failure_is_explicitly_uncertain_and_must_not_be_replayed() {
    let audit = Arc::new(RecordingAudit::default());
    audit.failures.lock().unwrap().extend([false, true]);
    let (runtime, _, calls) = runtime(audit.clone());
    let snapshot = runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::new(),
    );

    assert_eq!(
        snapshot.invoke("command", &json!({}), 1_000).await,
        Err(CapabilityRuntimeError::ExecutionOutcomeUncertain)
    );
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(audit.records.lock().unwrap().len(), 1);
}

#[test]
fn external_adapter_authorization_is_exact_and_terminal_audited() {
    let audit = Arc::new(RecordingAudit::default());
    let (runtime, _, calls) = runtime(audit.clone());
    let snapshot = runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::from(["workspace-a".into()]),
    );

    assert!(matches!(
        snapshot.authorize_external("unknown", &json!({}), "sha256:turn", 1_000),
        Err(CapabilityRuntimeError::CapabilityUnavailable)
    ));
    let lease = snapshot
        .authorize_external("command", &json!({}), "sha256:turn", 1_000)
        .unwrap();
    assert!(calls.lock().unwrap().is_empty());
    lease.finish(true).unwrap();

    let records = audit.records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].phase, CapabilityAuditPhase::Authorized);
    assert_eq!(records[0].capability_revision, "sha256:turn");
    assert_eq!(records[1].outcome, CapabilityAuditOutcome::Succeeded);
}

#[test]
fn external_adapter_pre_and_terminal_audit_failures_are_fail_closed() {
    let pre_audit = Arc::new(RecordingAudit::default());
    pre_audit.failures.lock().unwrap().push_back(true);
    let (capability_runtime, _, _) = runtime(pre_audit);
    let snapshot = capability_runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::new(),
    );
    assert!(matches!(
        snapshot.authorize_external("command", &json!({}), "sha256:turn", 1_000),
        Err(CapabilityRuntimeError::AuditUnavailable)
    ));

    let terminal_audit = Arc::new(RecordingAudit::default());
    terminal_audit
        .failures
        .lock()
        .unwrap()
        .extend([false, true]);
    let (capability_runtime, _, _) = runtime(terminal_audit.clone());
    let snapshot = capability_runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::new(),
    );
    let lease = snapshot
        .authorize_external("command", &json!({}), "sha256:turn", 1_000)
        .unwrap();
    assert_eq!(
        lease.finish(true),
        Err(CapabilityRuntimeError::ExecutionOutcomeUncertain)
    );
    assert_eq!(terminal_audit.records.lock().unwrap().len(), 1);
}

#[test]
fn dropping_external_adapter_lease_records_failed_terminal_without_execution() {
    let audit = Arc::new(RecordingAudit::default());
    let (runtime, _, calls) = runtime(audit.clone());
    let snapshot = runtime.begin_worker_run(
        &SessionContext::new("alice", "agent-a", "session-a"),
        BTreeSet::new(),
    );
    let lease = snapshot
        .authorize_external("command", &json!({}), "sha256:turn", 1_000)
        .unwrap();
    drop(lease);

    assert!(calls.lock().unwrap().is_empty());
    let records = audit.records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1].outcome, CapabilityAuditOutcome::Failed);
}

#[tokio::test]
async fn guardian_identity_is_distinct_expiring_and_has_no_worker_capabilities() {
    let audit = Arc::new(RecordingAudit::default());
    let (runtime, identity, calls) = runtime(audit);
    let owner = RuntimeOwnerScope::guardian(AgentId::new("agent-a"), None, BTreeSet::new());
    let snapshot = runtime
        .begin_guardian_run(&identity, owner.clone(), 1_999)
        .unwrap();
    assert_eq!(snapshot.actor(), CapabilityActor::Guardian);
    assert_eq!(
        snapshot
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>(),
        vec!["canonical_commit"]
    );
    assert_eq!(
        snapshot.invoke("command", &json!({}), 1_999).await,
        Err(CapabilityRuntimeError::CapabilityUnavailable)
    );
    assert_eq!(
        runtime.begin_guardian_run(&identity, owner, 2_000).err(),
        Some(CapabilityRuntimeError::AccessDenied)
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn guardian_registry_rejects_terminal_browser_host_and_arbitrary_mcp_classes() {
    for (index, class) in [
        CapabilityClass::Terminal,
        CapabilityClass::Browser,
        CapabilityClass::HostControl,
        CapabilityClass::ArbitraryMcp,
    ]
    .into_iter()
    .enumerate()
    {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let guardian = CapabilityRegistry::new()
            .register(RecordingCapability::new(
                &format!("dangerous-{index}"),
                class,
                calls,
            ))
            .unwrap();
        let identity = GuardianServiceIdentity::issue("guardian.curator", 1, 10).unwrap();
        assert_eq!(
            ActorCapabilityRuntime::new(
                CapabilityRegistry::new(),
                guardian,
                identity,
                1,
                Arc::new(RecordingAudit::default()),
            )
            .err(),
            Some(CapabilityRuntimeError::InvalidConfiguration)
        );
    }
}

#[test]
fn worker_and_guardian_registries_cannot_alias_the_same_route() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worker = CapabilityRegistry::new()
        .register(RecordingCapability::new(
            "shared",
            CapabilityClass::Read,
            calls.clone(),
        ))
        .unwrap();
    let guardian = CapabilityRegistry::new()
        .register(RecordingCapability::new(
            "shared",
            CapabilityClass::Read,
            calls,
        ))
        .unwrap();
    let identity = GuardianServiceIdentity::issue("guardian.curator", 1, 10).unwrap();
    assert_eq!(
        ActorCapabilityRuntime::new(
            worker,
            guardian,
            identity,
            1,
            Arc::new(RecordingAudit::default()),
        )
        .err(),
        Some(CapabilityRuntimeError::InvalidConfiguration)
    );
}
