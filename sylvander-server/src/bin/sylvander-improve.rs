//! Human-gated local administrator entry for Sylvander self-improvement.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use sylvander_runtime::evidence::{
    AnalysisPrivacyScope, CohortQuery, EvidenceStore, HmacSha256EvidenceSigner,
    ImprovementProposal, ImprovementProposalStatus, ProposalTransition, RequiredEvaluation,
};
use sylvander_runtime::self_change::{
    EvaluationMeasurements, SelfChangeEvaluationExecutor, SelfChangeExperimentManager,
};

#[tokio::main]
async fn main() {
    if let Err(error) = run(std::env::args().skip(1).collect()).await {
        eprintln!("sylvander-improve: {error}");
        std::process::exit(2);
    }
}

async fn run(arguments: Vec<String>) -> Result<(), String> {
    let (operation, options) = Options::parse(arguments)?;
    if operation == "help" {
        println!("{USAGE}");
        return Ok(());
    }
    let evidence = EvidenceStore::open(options.path("evidence")?)
        .await
        .map_err(display)?;
    match operation.as_str() {
        "analyze" => {
            let analysis = evidence
                .analyze_cohort(CohortQuery {
                    agent_id: options.optional("agent").map(str::to_owned),
                    started_at_inclusive: options.value("from")?,
                    started_before_exclusive: options.value("before")?,
                    privacy_scope: match options.required("privacy")? {
                        "shareable" => AnalysisPrivacyScope::ShareableOnly,
                        "private" => AnalysisPrivacyScope::IncludePrivate,
                        _ => return Err("--privacy must be shareable or private".into()),
                    },
                    limit: options.parse_or("limit", 500_u16)?,
                })
                .await
                .map_err(display)?;
            print_json(&analysis)
        }
        "proposal-create" => {
            let bytes = std::fs::read(options.path("definition")?).map_err(display)?;
            let proposal =
                serde_json::from_slice::<ImprovementProposal>(&bytes).map_err(display)?;
            print_json(
                &evidence
                    .register_improvement_proposal(proposal)
                    .await
                    .map_err(display)?,
            )
        }
        "proposal-transition" => {
            let status = proposal_status(options.required("status")?)?;
            print_json(
                &evidence
                    .transition_improvement_proposal(ProposalTransition {
                        proposal_id: options.required("proposal")?.into(),
                        expected_state_revision: options.value("revision")?,
                        status,
                        principal_digest: options.required("actor")?.into(),
                        reason: options.optional("reason").map(str::to_owned),
                        occurred_at: now()?,
                    })
                    .await
                    .map_err(display)?,
            )
        }
        "experiment-start" => {
            let manager = manager(&options, evidence)?;
            let started = manager
                .start(
                    options.required("experiment")?,
                    options.required("proposal")?,
                    &options.path("workspace")?,
                    options.required("actor")?,
                    now()?,
                )
                .await?;
            print_json(&serde_json::json!({
                "experiment": started.experiment,
                "worktree": started.lease.effective_workspace,
                "baseline_evidence": started.baseline_evidence,
            }))
        }
        "experiment-evaluate" => {
            let manager = manager(&options, evidence)?;
            print_json(
                &manager
                    .evaluate_candidate(
                        options.required("experiment")?,
                        options.required("actor")?,
                        now()?,
                    )
                    .await?
                    .experiment,
            )
        }
        "experiment-accept" => {
            let manager = manager(&options, evidence)?;
            let experiment = options.required("experiment")?;
            let actor = options.required("actor")?;
            manager
                .approve_merge(experiment, actor, options.required("reason")?, now()?)
                .await?;
            print_json(&manager.merge_approved(experiment, actor, now()?).await?)
        }
        "experiment-observe" => {
            let manager = manager(&options, evidence)?;
            print_json(
                &manager
                    .observe(
                        options.required("experiment")?,
                        options.required("actor")?,
                        now()?,
                    )
                    .await?,
            )
        }
        "experiment-rollback" => {
            let manager = manager(&options, evidence)?;
            print_json(
                &manager
                    .rollback(
                        options.required("experiment")?,
                        options.required("actor")?,
                        options.required("reason")?,
                        now()?,
                    )
                    .await?,
            )
        }
        _ => Err(format!("unknown operation `{operation}`\n\n{USAGE}")),
    }
}

fn manager(
    options: &Options,
    evidence: EvidenceStore,
) -> Result<SelfChangeExperimentManager, String> {
    let key = std::fs::read(options.path("signing-key-file")?).map_err(display)?;
    let signer = HmacSha256EvidenceSigner::new(
        options.optional("signing-key-id").unwrap_or("local-admin"),
        key,
    )
    .map_err(display)?;
    Ok(SelfChangeExperimentManager::new(
        evidence,
        options.path("worktree-root")?,
        Arc::new(CommandEvaluator {
            command: options.required("evaluation-command")?.into(),
        }),
        Arc::new(signer),
    ))
}

struct CommandEvaluator {
    command: String,
}

#[async_trait]
impl SelfChangeEvaluationExecutor for CommandEvaluator {
    async fn evaluate(
        &self,
        workspace: &Path,
        required: &[RequiredEvaluation],
    ) -> Result<Vec<EvaluationMeasurements>, String> {
        let command = self.command.clone();
        let workspace = workspace.to_path_buf();
        let required = required.to_vec();
        tokio::task::spawn_blocking(move || run_evaluation(&command, &workspace, &required))
            .await
            .map_err(display)?
    }
}

fn run_evaluation(
    command: &str,
    workspace: &Path,
    required: &[RequiredEvaluation],
) -> Result<Vec<EvaluationMeasurements>, String> {
    let output = std::process::Command::new("/bin/sh")
        .args(["-lc", command])
        .current_dir(workspace)
        .env(
            "SYLVANDER_REQUIRED_EVALUATIONS",
            serde_json::to_string(required).map_err(display)?,
        )
        .output()
        .map_err(display)?;
    if !output.status.success() {
        return Err(format!(
            "evaluation command failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(2_048)
                .collect::<String>()
        ));
    }
    if output.stdout.len() > 1024 * 1024 {
        return Err("evaluation output exceeds 1 MiB".into());
    }
    let measurements =
        serde_json::from_slice::<Vec<EvaluationMeasurements>>(&output.stdout).map_err(display)?;
    let expected = required
        .iter()
        .map(|item| item.baseline_id.as_str())
        .collect::<BTreeSet<_>>();
    let actual = measurements
        .iter()
        .map(|item| item.baseline_id.as_str())
        .collect::<BTreeSet<_>>();
    if actual != expected || actual.len() != measurements.len() {
        return Err("evaluation output must contain each required baseline exactly once".into());
    }
    Ok(measurements)
}

fn proposal_status(value: &str) -> Result<ImprovementProposalStatus, String> {
    match value {
        "ready_for_review" => Ok(ImprovementProposalStatus::ReadyForReview),
        "approved" => Ok(ImprovementProposalStatus::Approved),
        "rejected" => Ok(ImprovementProposalStatus::Rejected),
        _ => Err("--status must be ready_for_review, approved, or rejected".into()),
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    println!("{}", serde_json::to_string_pretty(value).map_err(display)?);
    Ok(())
}

fn now() -> Result<i64, String> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(display)?
        .as_secs();
    i64::try_from(seconds).map_err(display)
}

fn display(error: impl std::fmt::Display) -> String {
    error.to_string()
}

struct Options {
    values: BTreeMap<String, String>,
}

impl Options {
    fn parse(arguments: Vec<String>) -> Result<(String, Self), String> {
        let mut arguments = arguments.into_iter();
        let operation = arguments.next().unwrap_or_else(|| "help".into());
        let mut values = BTreeMap::new();
        while let Some(flag) = arguments.next() {
            let name = flag
                .strip_prefix("--")
                .ok_or_else(|| format!("expected --option, found `{flag}`"))?;
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for --{name}"))?;
            if values.insert(name.into(), value).is_some() {
                return Err(format!("duplicate --{name}"));
            }
        }
        Ok((operation, Self { values }))
    }

    fn required(&self, name: &str) -> Result<&str, String> {
        self.optional(name)
            .ok_or_else(|| format!("missing --{name}"))
    }

    fn optional(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    fn path(&self, name: &str) -> Result<PathBuf, String> {
        self.required(name).map(PathBuf::from)
    }

    fn value<T>(&self, name: &str) -> Result<T, String>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        self.required(name)?
            .parse()
            .map_err(|error| format!("invalid --{name}: {error}"))
    }

    fn parse_or<T>(&self, name: &str, default: T) -> Result<T, String>
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        match self.optional(name) {
            Some(value) => value
                .parse()
                .map_err(|error| format!("invalid --{name}: {error}")),
            None => Ok(default),
        }
    }
}

const USAGE: &str = r#"Usage:
  sylvander-improve analyze --evidence DB --from UNIX --before UNIX --privacy shareable|private [--agent ID] [--limit N]
  sylvander-improve proposal-create --evidence DB --definition proposal.json
  sylvander-improve proposal-transition --evidence DB --proposal ID --revision N --status ready_for_review|approved|rejected --actor SHA256 [--reason TEXT]
  sylvander-improve experiment-start --evidence DB --proposal ID --experiment ID --workspace REPO --actor SHA256 <experiment options>
  sylvander-improve experiment-evaluate --evidence DB --experiment ID --actor SHA256 <experiment options>
  sylvander-improve experiment-accept --evidence DB --experiment ID --actor SHA256 --reason TEXT <experiment options>
  sylvander-improve experiment-observe --evidence DB --experiment ID --actor SHA256 <experiment options>
  sylvander-improve experiment-rollback --evidence DB --experiment ID --actor SHA256 --reason TEXT <experiment options>

Experiment options:
  --worktree-root PATH --evaluation-command SHELL --signing-key-file PATH [--signing-key-id ID]

The evaluation command runs inside the source/worktree and receives
SYLVANDER_REQUIRED_EVALUATIONS as JSON. It must print a JSON array of
{"baseline_id":"...","measurements":[{"metric":"...","value":N,"sample_count":N}]}.
"#;
