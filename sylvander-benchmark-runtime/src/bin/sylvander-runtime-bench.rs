use std::path::Path;

use serde::Serialize;
use sylvander_benchmark_runtime::{
    ActivationGatePolicy, AppendOutcome, BenchmarkLedger, CorpusManifest, RuntimeBenchPlan,
    RuntimeBenchResult, evaluate_corpus_activation, summarize,
};

const MAX_JSON_BYTES: u64 = 64 * 1024 * 1024;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [command, path] if command == "validate-plan" => validate_plan(path),
        [command, path] if command == "validate-corpus" => validate_corpus(path),
        [command, path] if command == "plan-corpus" => plan_corpus(path),
        [command, manifest, baseline, candidate, policy] if command == "evaluate-corpus" => {
            evaluate_corpus(manifest, baseline, candidate, policy)
        }
        [command, path] if command == "summarize" => summarize_json(path),
        [command, ledger] if command == "summarize-ledger" => summarize_ledger(ledger),
        [command, ledger, results] if command == "record" => record(ledger, results),
        [command, ledger, plan] if command == "coverage" => coverage(ledger, plan),
        _ => Err(usage()),
    }
}

fn read_corpus(path: &str) -> Result<CorpusManifest, String> {
    let bytes = read_bytes(path, "corpus manifest")?;
    CorpusManifest::from_json(&bytes).map_err(|error| error.to_string())
}

fn validate_corpus(path: &str) -> Result<(), String> {
    let manifest = read_corpus(path)?;
    manifest
        .verify_artifacts(path)
        .map_err(|error| error.to_string())?;
    let (_, digest) = manifest
        .canonical_json_and_sha256()
        .map_err(|error| error.to_string())?;
    println!(
        "valid digest={digest} scenarios={} paired_coordinates={}",
        manifest.scenarios.len(),
        manifest
            .scenarios
            .len()
            .saturating_mul(manifest.repetitions as usize)
            .saturating_mul(2)
    );
    Ok(())
}

#[derive(Serialize)]
struct PairedPlans {
    baseline: RuntimeBenchPlan,
    candidate: RuntimeBenchPlan,
}

fn plan_corpus(path: &str) -> Result<(), String> {
    let manifest = read_corpus(path)?;
    manifest
        .verify_artifacts(path)
        .map_err(|error| error.to_string())?;
    let (baseline, candidate) = manifest.paired_plans().map_err(|error| error.to_string())?;
    print_json(&PairedPlans {
        baseline,
        candidate,
    })
}

fn evaluate_corpus(
    manifest_path: &str,
    baseline_path: &str,
    candidate_path: &str,
    policy_path: &str,
) -> Result<(), String> {
    let manifest = read_corpus(manifest_path)?;
    manifest
        .verify_artifacts(manifest_path)
        .map_err(|error| error.to_string())?;
    let baseline: Vec<RuntimeBenchResult> = read_json(baseline_path, "baseline results")?;
    let candidate: Vec<RuntimeBenchResult> = read_json(candidate_path, "candidate results")?;
    let policy: ActivationGatePolicy = read_json(policy_path, "activation policy")?;
    let bundle = evaluate_corpus_activation(&manifest, &baseline, &candidate, policy)
        .map_err(|error| error.to_string())?;
    print_json(&bundle)
}

fn validate_plan(path: &str) -> Result<(), String> {
    let plan: RuntimeBenchPlan = read_json(path, "benchmark plan")?;
    plan.validate().map_err(|error| error.to_string())?;
    println!("valid coordinates={}", plan.coordinates.len());
    Ok(())
}

fn summarize_json(path: &str) -> Result<(), String> {
    let results: Vec<RuntimeBenchResult> = read_json(path, "benchmark results")?;
    print_json(&summarize(&results).map_err(|error| error.to_string())?)
}

fn summarize_ledger(path: &str) -> Result<(), String> {
    let ledger = BenchmarkLedger::open(path).map_err(|error| error.to_string())?;
    let results = ledger.results().map_err(|error| error.to_string())?;
    print_json(&summarize(&results).map_err(|error| error.to_string())?)
}

fn record(ledger_path: &str, results_path: &str) -> Result<(), String> {
    let results: Vec<RuntimeBenchResult> = read_json(results_path, "benchmark results")?;
    let mut ledger = BenchmarkLedger::open(ledger_path).map_err(|error| error.to_string())?;
    let mut inserted = 0_u64;
    let mut already_present = 0_u64;
    for result in &results {
        match ledger.append(result).map_err(|error| error.to_string())? {
            AppendOutcome::Inserted => inserted = inserted.saturating_add(1),
            AppendOutcome::AlreadyPresent => already_present = already_present.saturating_add(1),
        }
    }
    println!("recorded inserted={inserted} already_present={already_present}");
    Ok(())
}

fn coverage(ledger_path: &str, plan_path: &str) -> Result<(), String> {
    let plan: RuntimeBenchPlan = read_json(plan_path, "benchmark plan")?;
    let ledger = BenchmarkLedger::open(ledger_path).map_err(|error| error.to_string())?;
    print_json(&ledger.coverage(&plan).map_err(|error| error.to_string())?)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &str, kind: &str) -> Result<T, String> {
    let bytes = read_bytes(path, kind)?;
    serde_json::from_slice(&bytes).map_err(|_| format!("invalid {kind} JSON"))
}

fn read_bytes(path: &str, kind: &str) -> Result<Vec<u8>, String> {
    let path = Path::new(path);
    let metadata = std::fs::metadata(path).map_err(|_| format!("cannot read {kind}"))?;
    if !metadata.is_file() || metadata.len() > MAX_JSON_BYTES {
        return Err(format!("invalid {kind} file"));
    }
    std::fs::read(path).map_err(|_| format!("cannot read {kind}"))
}

fn print_json(value: &impl serde::Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|_| "cannot encode benchmark output".to_owned())?
    );
    Ok(())
}

fn usage() -> String {
    "usage: sylvander-runtime-bench \
     <validate-plan PLAN|validate-corpus MANIFEST|plan-corpus MANIFEST|\
     evaluate-corpus MANIFEST BASELINE CANDIDATE POLICY|summarize RESULTS|\
     summarize-ledger DB|record DB RESULTS|coverage DB PLAN>"
        .into()
}
