use std::path::Path;

use sylvander_benchmark_runtime::{
    AppendOutcome, BenchmarkLedger, RuntimeBenchPlan, RuntimeBenchResult, summarize,
};

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
        [command, path] if command == "summarize" => summarize_json(path),
        [command, ledger] if command == "summarize-ledger" => summarize_ledger(ledger),
        [command, ledger, results] if command == "record" => record(ledger, results),
        [command, ledger, plan] if command == "coverage" => coverage(ledger, plan),
        _ => Err(usage()),
    }
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
    let bytes = std::fs::read(Path::new(path)).map_err(|_| format!("cannot read {kind}"))?;
    serde_json::from_slice(&bytes).map_err(|_| format!("invalid {kind} JSON"))
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
     <validate-plan PLAN|summarize RESULTS|summarize-ledger DB|record DB RESULTS|coverage DB PLAN>"
        .into()
}
