use super::*;
use crate::{
    ActivationDecision, ActivationGatePolicy, BenchmarkModelBinding, BenchmarkModelRole,
    FailurePoint, RuntimeBenchCoordinate, TopologyProfile, WorkspaceProfile,
    evaluate_corpus_activation,
};

fn manifest() -> CorpusManifest {
    CorpusManifest {
        schema_version: CORPUS_MANIFEST_SCHEMA_VERSION,
        suite: "perception".into(),
        suite_revision: "2026-08-14".into(),
        candidate: CognitionProfile::PerceptionSpecialist,
        primary_model: "primary@1".into(),
        auxiliary_models: vec![BenchmarkModelBinding {
            role: BenchmarkModelRole::Audio,
            model: "audio@1".into(),
        }],
        repetitions: 2,
        scenarios: vec![scenario("audio-001", CorpusModality::Audio)],
    }
}

fn scenario(id: &str, modality: CorpusModality) -> CorpusScenario {
    CorpusScenario {
        id: id.into(),
        family: ScenarioFamily::MultimodalPerception,
        modality,
        risk: CorpusRisk::Medium,
        input: CorpusArtifact {
            locator: format!("fixtures/{id}.bin"),
            media_type: "application/octet-stream".into(),
            sha256: "a".repeat(64),
        },
        verifier: CorpusVerifier {
            id: "exact-answer".into(),
            revision: "1".into(),
            sha256: "b".repeat(64),
        },
        provenance: CorpusProvenance {
            dataset: "internal-safe".into(),
            revision: "1".into(),
            license: "Apache-2.0".into(),
        },
    }
}

fn result(profile: CognitionProfile, run_ordinal: u32) -> RuntimeBenchResult {
    let mut models = vec![BenchmarkModelBinding {
        role: BenchmarkModelRole::Primary,
        model: "primary@1".into(),
    }];
    let candidate = profile == CognitionProfile::PerceptionSpecialist;
    if candidate {
        models.push(BenchmarkModelBinding {
            role: BenchmarkModelRole::Audio,
            model: "audio@1".into(),
        });
    }
    RuntimeBenchResult {
        coordinate: RuntimeBenchCoordinate {
            suite: "perception".into(),
            suite_revision: "2026-08-14".into(),
            scenario_id: "audio-001".into(),
            family: ScenarioFamily::MultimodalPerception,
            topology: TopologyProfile::SingleAgent,
            workspace: WorkspaceProfile::ReadOnlyShared,
            failure_point: FailurePoint::None,
            cognition: profile,
            models,
            run_ordinal,
        },
        verifier_reward: Some(1.0),
        useful_completion: true,
        invariant_violations: 0,
        duplicate_effects: 0,
        user_visible_failures: 0,
        recovered: false,
        duration_millis: 10,
        input_tokens: 10,
        output_tokens: 10,
        model_calls: if candidate { 2 } else { 1 },
        primary_model_calls: 1,
        auxiliary_model_calls: u32::from(candidate),
        perception_calls: u32::from(candidate),
        cognitive_fallbacks: 0,
        tool_calls: 0,
        messages: 0,
        handoffs: 0,
        moderator_interventions: 0,
        workspace_conflicts: 0,
        doctor_findings: 0,
        doctor_false_positives: 0,
        doctor_proposals: 0,
        doctor_auto_applied: 0,
    }
}

#[test]
fn canonical_manifest_round_trips_with_a_stable_digest() {
    let manifest = manifest();
    let (json, digest) = manifest.canonical_json_and_sha256().unwrap();
    let decoded = CorpusManifest::from_json(json.as_bytes()).unwrap();
    assert_eq!(decoded, manifest);
    assert_eq!(decoded.canonical_json_and_sha256().unwrap().1, digest);
    assert_eq!(digest.len(), 64);
}

#[test]
fn manifest_rejects_unordered_scenarios_bad_hashes_and_text_perception() {
    let mut invalid = manifest();
    invalid
        .scenarios
        .push(scenario("aaa", CorpusModality::Audio));
    assert!(matches!(
        invalid.validate(),
        Err(CorpusManifestError::UnsortedOrDuplicateScenario)
    ));
    let mut invalid = manifest();
    invalid.scenarios[0].input.sha256 = "not-a-digest".into();
    assert!(matches!(
        invalid.validate(),
        Err(CorpusManifestError::InvalidScenario)
    ));
    let mut invalid = manifest();
    invalid.scenarios[0].modality = CorpusModality::Text;
    assert!(matches!(
        invalid.validate(),
        Err(CorpusManifestError::InvalidScenario)
    ));
}

#[test]
fn paired_coverage_requires_every_declared_run_exactly_once() {
    let manifest = manifest();
    let baseline = vec![
        result(CognitionProfile::PrimaryOnly, 1),
        result(CognitionProfile::PrimaryOnly, 2),
    ];
    let candidate = vec![
        result(CognitionProfile::PerceptionSpecialist, 1),
        result(CognitionProfile::PerceptionSpecialist, 2),
    ];
    manifest
        .validate_pair_coverage(&baseline, &candidate)
        .unwrap();
    assert!(matches!(
        manifest.validate_pair_coverage(&baseline[..1], &candidate),
        Err(CorpusManifestError::IncompletePairCoverage)
    ));
    let duplicated = vec![baseline[0].clone(), baseline[0].clone()];
    assert!(matches!(
        manifest.validate_pair_coverage(&duplicated, &candidate),
        Err(CorpusManifestError::UnexpectedResult)
    ));
}

#[test]
fn manifest_expands_exact_primary_and_candidate_arms() {
    let (baseline, candidate) = manifest().paired_plans().unwrap();
    assert_eq!(baseline.coordinates.len(), 2);
    assert_eq!(candidate.coordinates.len(), 2);
    assert_eq!(baseline.coordinates[0].models.len(), 1);
    assert_eq!(candidate.coordinates[0].models.len(), 2);
    assert_eq!(baseline.coordinates[0].models[0].model, "primary@1");
    assert_eq!(candidate.coordinates[0].models[0].model, "primary@1");
}

#[test]
fn artifact_verification_rejects_changed_bytes_and_path_escape() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("fixtures")).unwrap();
    std::fs::write(directory.path().join("fixtures/audio-001.bin"), b"audio").unwrap();
    let manifest_path = directory.path().join("manifest.json");
    std::fs::write(&manifest_path, b"{}").unwrap();
    let mut manifest = manifest();
    manifest.scenarios[0].input.sha256 =
        "6ed8919ce20490a5e3ad8630a4fab69475297abd07db73918dd5f36fcfaeb11b".into();
    manifest.verify_artifacts(&manifest_path).unwrap();

    std::fs::write(directory.path().join("fixtures/audio-001.bin"), b"changed").unwrap();
    assert!(matches!(
        manifest.verify_artifacts(&manifest_path),
        Err(CorpusManifestError::ArtifactDigestMismatch)
    ));
    manifest.scenarios[0].input.locator = "../outside".into();
    assert!(matches!(
        manifest.verify_artifacts(&manifest_path),
        Err(CorpusManifestError::UnsafeArtifactPath)
    ));
}

#[test]
fn corpus_evidence_digest_is_stable_across_result_input_order() {
    let baseline = vec![
        result(CognitionProfile::PrimaryOnly, 1),
        result(CognitionProfile::PrimaryOnly, 2),
    ];
    let candidate = vec![
        result(CognitionProfile::PerceptionSpecialist, 1),
        result(CognitionProfile::PerceptionSpecialist, 2),
    ];
    let policy = ActivationGatePolicy {
        minimum_pairs: 2,
        minimum_reward_gain_micros: 0,
        minimum_quality_win_basis_points: 0,
        maximum_token_increase_basis_points: 10_000,
        maximum_p95_latency_increase_basis_points: 10_000,
    };
    let first = evaluate_corpus_activation(&manifest(), &baseline, &candidate, policy).unwrap();
    let second = evaluate_corpus_activation(
        &manifest(),
        &baseline.into_iter().rev().collect::<Vec<_>>(),
        &candidate.into_iter().rev().collect::<Vec<_>>(),
        policy,
    )
    .unwrap();
    assert_eq!(first.report.decision, ActivationDecision::Eligible);
    assert_eq!(first.evidence, second.evidence);
    assert_eq!(first.evidence.evidence_set_sha256.len(), 64);
}
