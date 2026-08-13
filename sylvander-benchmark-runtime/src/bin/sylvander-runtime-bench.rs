use std::path::Path;

use sylvander_benchmark_runtime::{RuntimeBenchPlan, RuntimeBenchResult, summarize};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments.next().ok_or_else(usage)?;
    let path = arguments.next().ok_or_else(usage)?;
    if arguments.next().is_some() {
        return Err(usage());
    }
    let bytes = std::fs::read(Path::new(&path))
        .map_err(|error| format!("cannot read benchmark artifact: {error}"))?;
    match command.as_str() {
        "validate-plan" => {
            let plan: RuntimeBenchPlan = serde_json::from_slice(&bytes)
                .map_err(|error| format!("invalid benchmark plan JSON: {error}"))?;
            plan.validate().map_err(|error| error.to_string())?;
            println!("valid coordinates={}", plan.coordinates.len());
        }
        "summarize" => {
            let results: Vec<RuntimeBenchResult> = serde_json::from_slice(&bytes)
                .map_err(|error| format!("invalid benchmark result JSON: {error}"))?;
            let summary = summarize(&results).map_err(|error| error.to_string())?;
            println!(
                "{}",
                serde_json::to_string_pretty(&summary)
                    .map_err(|error| format!("cannot encode summary: {error}"))?
            );
        }
        _ => return Err(usage()),
    }
    Ok(())
}

fn usage() -> String {
    "usage: sylvander-runtime-bench <validate-plan|summarize> <json-path>".into()
}
