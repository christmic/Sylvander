use super::*;

fn coordinate() -> RuntimeBenchCoordinate {
    RuntimeBenchCoordinate {
        suite: "local-faults".into(),
        suite_revision: "v1".into(),
        scenario_id: "tool-effect-started".into(),
        family: ScenarioFamily::CrashRecovery,
        topology: TopologyProfile::SingleAgent,
        workspace: WorkspaceProfile::IsolatedWorktrees,
        failure_point: FailurePoint::ToolStarted,
        cognition: CognitionProfile::PrimaryOnly,
        models: vec![BenchmarkModelBinding {
            role: BenchmarkModelRole::Primary,
            model: "provider/model".into(),
        }],
        run_ordinal: 1,
    }
}

#[test]
fn ledger_is_write_once_and_exact_retries_are_idempotent() {
    let mut ledger = BenchmarkLedger::open_in_memory().unwrap();
    let result = benchmark_result(coordinate());
    assert_eq!(ledger.append(&result).unwrap(), AppendOutcome::Inserted);
    assert_eq!(
        ledger.append(&result).unwrap(),
        AppendOutcome::AlreadyPresent
    );

    let mut conflicting = result;
    conflicting.output_tokens += 1;
    assert_eq!(
        ledger.append(&conflicting),
        Err(BenchmarkLedgerError::ConflictingResult)
    );
}

#[test]
fn coverage_exposes_missing_and_unexpected_cells() {
    let mut ledger = BenchmarkLedger::open_in_memory().unwrap();
    let expected = coordinate();
    let mut missing = expected.clone();
    missing.run_ordinal = 2;
    let mut unexpected = expected.clone();
    unexpected.scenario_id = "unexpected".into();
    ledger.append(&benchmark_result(expected.clone())).unwrap();
    ledger.append(&benchmark_result(unexpected)).unwrap();

    let coverage = ledger
        .coverage(&RuntimeBenchPlan {
            schema_version: 2,
            coordinates: vec![expected, missing],
        })
        .unwrap();
    assert_eq!(coverage.expected, 2);
    assert_eq!(coverage.recorded, 2);
    assert_eq!(coverage.missing, 1);
    assert_eq!(coverage.unexpected, 1);
    assert!(!coverage.is_complete());
}

#[test]
fn file_ledger_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("runtime-bench.sqlite3");
    let result = benchmark_result(coordinate());
    BenchmarkLedger::open(&path)
        .unwrap()
        .append(&result)
        .unwrap();
    assert_eq!(
        BenchmarkLedger::open(path).unwrap().results().unwrap(),
        vec![result]
    );
}

fn benchmark_result(coordinate: RuntimeBenchCoordinate) -> RuntimeBenchResult {
    RuntimeBenchResult {
        coordinate,
        verifier_reward: None,
        useful_completion: true,
        invariant_violations: 0,
        duplicate_effects: 0,
        user_visible_failures: 0,
        recovered: true,
        duration_millis: 10,
        input_tokens: 20,
        output_tokens: 5,
        model_calls: 1,
        primary_model_calls: 1,
        auxiliary_model_calls: 0,
        perception_calls: 0,
        cognitive_fallbacks: 0,
        tool_calls: 1,
        messages: 1,
        handoffs: 0,
        moderator_interventions: 0,
        workspace_conflicts: 0,
        doctor_findings: 0,
        doctor_false_positives: 0,
        doctor_proposals: 0,
        doctor_auto_applied: 0,
    }
}
