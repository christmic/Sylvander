use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use sylvander_protocol::EvidenceReference;
use sylvander_runtime::evidence::{
    EvaluationBaseline, EvaluationCase, EvaluationDatasetRevision, EvaluationSplit, EvidenceStore,
    ImprovementProposal, ImprovementProposalStatus, ImprovementRisk, RegressionMetric,
    RequiredEvaluation, ScoreDirection, ScoringAdapterKind, ScoringAdapterRevision,
    SelfChangeExperimentStatus, TurnStart,
};

#[test]
fn administrator_binary_exposes_the_gated_workflow() {
    let output = Command::new(env!("CARGO_BIN_EXE_sylvander-improve"))
        .arg("help")
        .output()
        .expect("run administrator binary");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for operation in [
        "analyze",
        "proposal-create",
        "experiment-start",
        "experiment-evaluate",
        "experiment-accept",
        "experiment-observe",
        "experiment-rollback",
    ] {
        assert!(stdout.contains(operation), "{operation}");
    }
}

#[tokio::test]
async fn analyze_command_reads_the_durable_evidence_store() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("evidence.sqlite");
    let store = EvidenceStore::open(&database).await.unwrap();
    store
        .start_run("run-1".into(), "test".into(), 1)
        .await
        .unwrap();
    store
        .start_turn(TurnStart {
            id: "turn-1".into(),
            run_id: "run-1".into(),
            session_id: "session-1".into(),
            agent_id: Some("agent-1".into()),
            started_at: 2,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .unwrap();
    store
        .record_outcome("outcome-1".into(), "turn-1".into(), "done".into(), true, 3)
        .await
        .unwrap();
    store
        .finish_turn("turn-1".into(), 3, "succeeded", 0)
        .await
        .unwrap();
    drop(store);

    let output = Command::new(env!("CARGO_BIN_EXE_sylvander-improve"))
        .args([
            "analyze",
            "--evidence",
            database.to_str().unwrap(),
            "--from",
            "0",
            "--before",
            "10",
            "--privacy",
            "private",
        ])
        .output()
        .expect("run analysis");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["turns"][0]["id"], "turn-1");
    assert_eq!(json["succeeded_turns"], 1);
}

#[tokio::test]
async fn administrator_binary_runs_real_git_observe_and_rollback_journeys() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init"]);
    git(&source, &["config", "user.name", "Sylvander Test"]);
    git(
        &source,
        &["config", "user.email", "sylvander-test@example.com"],
    );
    fs::write(source.join("README.md"), "base\n").unwrap();
    git(&source, &["add", "."]);
    git(&source, &["commit", "-m", "base"]);

    let database = temporary.path().join("evidence.sqlite");
    seed_evaluation_registry(&database).await;
    let journey = Journey::new(temporary.path(), source.clone(), database.clone());

    journey.proposal("proposal-observe");
    let observed_worktree = journey.start("proposal-observe", "experiment-observe");
    fs::write(observed_worktree.join("observed.txt"), "candidate\n").unwrap();
    journey.evaluate("experiment-observe");
    journey.accept("experiment-observe");
    assert_eq!(
        fs::read_to_string(source.join("observed.txt")).unwrap(),
        "candidate\n"
    );
    journey.observe("experiment-observe");

    journey.proposal("proposal-rollback");
    let rollback_worktree = journey.start("proposal-rollback", "experiment-rollback");
    fs::write(rollback_worktree.join("rolled-back.txt"), "candidate\n").unwrap();
    journey.evaluate("experiment-rollback");
    journey.accept("experiment-rollback");
    assert!(source.join("rolled-back.txt").is_file());
    journey.rollback("experiment-rollback");
    assert!(!source.join("rolled-back.txt").exists());
    assert!(source.join("observed.txt").is_file());
    assert!(git_text(&source, &["status", "--porcelain"]).is_empty());

    let store = EvidenceStore::open(&database).await.unwrap();
    assert_eq!(
        store
            .improvement_proposal("proposal-observe".into())
            .await
            .unwrap()
            .unwrap()
            .status,
        ImprovementProposalStatus::Completed
    );
    assert_eq!(
        store
            .improvement_proposal("proposal-rollback".into())
            .await
            .unwrap()
            .unwrap()
            .status,
        ImprovementProposalStatus::RolledBack
    );
    assert_eq!(
        store
            .self_change_experiment("experiment-observe".into())
            .await
            .unwrap()
            .unwrap()
            .status,
        SelfChangeExperimentStatus::Completed
    );
    assert_eq!(
        store
            .self_change_experiment("experiment-rollback".into())
            .await
            .unwrap()
            .unwrap()
            .status,
        SelfChangeExperimentStatus::RolledBack
    );
}

struct Journey {
    root: std::path::PathBuf,
    source: std::path::PathBuf,
    database: std::path::PathBuf,
    worktree_root: std::path::PathBuf,
    signing_key: std::path::PathBuf,
    actor: String,
}

impl Journey {
    fn new(root: &Path, source: std::path::PathBuf, database: std::path::PathBuf) -> Self {
        let signing_key = root.join("signing.key");
        fs::write(&signing_key, [7_u8; 32]).unwrap();
        Self {
            root: root.into(),
            source,
            database,
            worktree_root: root.join("runtime"),
            signing_key,
            actor: "9".repeat(64),
        }
    }

    fn proposal(&self, proposal_id: &str) {
        let definition = self.root.join(format!("{proposal_id}.json"));
        let proposal = ImprovementProposal {
            id: proposal_id.into(),
            cohort_digest_sha256: "b".repeat(64),
            evidence: vec![reference("turn")],
            hypothesis: "Improve the local runtime.".into(),
            expected_benefit: "Higher task completion.".into(),
            risk: ImprovementRisk::Medium,
            affected_components: vec!["runtime".into()],
            rollback_plan: "Revert the reviewed merge.".into(),
            required_evaluations: vec![RequiredEvaluation {
                dataset_id: "coding".into(),
                dataset_revision: 1,
                baseline_id: "coding-main".into(),
            }],
            created_by_principal_digest: "8".repeat(64),
            created_at: 4,
        };
        fs::write(&definition, serde_json::to_vec_pretty(&proposal).unwrap()).unwrap();
        assert!(
            self.run(
                "proposal-create",
                &[("definition", path(&definition))],
                false,
            )
            .is_string()
        );
        let ready = self.run(
            "proposal-transition",
            &[
                ("proposal", proposal_id),
                ("revision", "1"),
                ("status", "ready_for_review"),
                ("actor", &self.actor),
                ("reason", "Ready for independent review."),
            ],
            false,
        );
        assert_eq!(ready["status"], "ready_for_review");
        assert_eq!(ready["state_revision"], 2);
        let approved = self.run(
            "proposal-transition",
            &[
                ("proposal", proposal_id),
                ("revision", "2"),
                ("status", "approved"),
                ("actor", &self.actor),
                ("reason", "Approved for isolated evaluation."),
            ],
            false,
        );
        assert_eq!(approved["status"], "approved");
        assert_eq!(approved["state_revision"], 3);
    }

    fn start(&self, proposal_id: &str, experiment_id: &str) -> std::path::PathBuf {
        let started = self.run(
            "experiment-start",
            &[
                ("proposal", proposal_id),
                ("experiment", experiment_id),
                ("workspace", path(&self.source)),
                ("actor", &self.actor),
            ],
            true,
        );
        assert_eq!(started["experiment"]["status"], "prepared");
        started["worktree"].as_str().unwrap().into()
    }

    fn evaluate(&self, experiment_id: &str) {
        let evaluated = self.run(
            "experiment-evaluate",
            &[("experiment", experiment_id), ("actor", &self.actor)],
            true,
        );
        assert_eq!(evaluated["status"], "candidate_evaluated");
    }

    fn accept(&self, experiment_id: &str) {
        let merged = self.run(
            "experiment-accept",
            &[
                ("experiment", experiment_id),
                ("actor", &self.actor),
                ("reason", "Operator reviewed the candidate evidence."),
            ],
            true,
        );
        assert_eq!(merged["status"], "merged");
    }

    fn observe(&self, experiment_id: &str) {
        let observed = self.run(
            "experiment-observe",
            &[("experiment", experiment_id), ("actor", &self.actor)],
            true,
        );
        assert_eq!(observed["status"], "completed");
    }

    fn rollback(&self, experiment_id: &str) {
        let rolled_back = self.run(
            "experiment-rollback",
            &[
                ("experiment", experiment_id),
                ("actor", &self.actor),
                ("reason", "Operator rejected the merged candidate."),
            ],
            true,
        );
        assert_eq!(rolled_back["status"], "rolled_back");
    }

    fn run(
        &self,
        operation: &str,
        options: &[(&str, &str)],
        experiment_options: bool,
    ) -> serde_json::Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_sylvander-improve"));
        command
            .arg(operation)
            .args(["--evidence", path(&self.database)]);
        for (name, value) in options {
            command.arg(format!("--{name}")).arg(value);
        }
        if experiment_options {
            command
                .args(["--worktree-root", path(&self.worktree_root)])
                .args(["--evaluation-command", EVALUATOR])
                .args(["--signing-key-file", path(&self.signing_key)]);
        }
        let output = command.output().expect("run administrator binary");
        assert_success(&[operation], &output);
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

async fn seed_evaluation_registry(database: &Path) {
    let store = EvidenceStore::open(database).await.unwrap();
    store
        .register_scoring_adapter(ScoringAdapterRevision {
            id: "validation".into(),
            revision: 1,
            kind: ScoringAdapterKind::BooleanValidation,
            metric: "passed".into(),
            config_digest_sha256: "a".repeat(64),
            created_at: 1,
        })
        .await
        .unwrap();
    store
        .register_evaluation_dataset(EvaluationDatasetRevision {
            id: "coding".into(),
            revision: 1,
            name: "Coding".into(),
            cases: vec![
                EvaluationCase {
                    id: "fixture".into(),
                    split: EvaluationSplit::Fixture,
                    input: reference("fixture"),
                    expected: None,
                    scorer_id: "validation".into(),
                    scorer_revision: 1,
                },
                EvaluationCase {
                    id: "held".into(),
                    split: EvaluationSplit::HeldOut,
                    input: reference("held"),
                    expected: None,
                    scorer_id: "validation".into(),
                    scorer_revision: 1,
                },
            ],
            created_at: 2,
        })
        .await
        .unwrap();
    store
        .register_evaluation_baseline(EvaluationBaseline {
            id: "coding-main".into(),
            dataset_id: "coding".into(),
            dataset_revision: 1,
            metrics: vec![RegressionMetric {
                metric: "passed".into(),
                direction: ScoreDirection::HigherIsBetter,
                baseline_value: 100,
                sample_count: 1,
                max_regression_basis_points: 0,
            }],
            recorded_at: 3,
        })
        .await
        .unwrap();
}

fn path(value: &Path) -> &str {
    value.to_str().unwrap()
}

fn reference(name: &str) -> EvidenceReference {
    EvidenceReference {
        locator: name.into(),
        digest_sha256: Some("c".repeat(64)),
    }
}

fn git(cwd: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert_success(arguments, &output);
}

fn git_text(cwd: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert_success(arguments, &output);
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn assert_success(arguments: &[&str], output: &Output) {
    assert!(
        output.status.success(),
        "{} failed with {}: {}",
        arguments.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

const EVALUATOR: &str = r#"printf '%s\n' '[{"baseline_id":"coding-main","measurements":[{"metric":"passed","value":100,"sample_count":1}]}]'"#;
