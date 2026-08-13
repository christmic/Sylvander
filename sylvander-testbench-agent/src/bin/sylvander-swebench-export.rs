//! Export one completed workspace in the official SWE-bench prediction shape.

use std::env;
use std::fs::File;
use std::path::PathBuf;

use sylvander_testbench_agent::swebench::SweBenchPrediction;

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("sylvander-swebench-export: {error}");
        std::process::exit(1);
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let value = |name: &str| {
        arguments
            .windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].clone())
            .ok_or_else(|| format!("missing required argument {name}"))
    };
    let workspace = PathBuf::from(value("--workspace")?);
    let instance = value("--instance-id")?;
    let model = value("--model")?;
    let output = PathBuf::from(value("--output")?);
    let prediction = SweBenchPrediction::from_workspace(&workspace, instance, model)?;
    let file = File::create(&output)
        .map_err(|error| format!("cannot create prediction {}: {error}", output.display()))?;
    serde_json::to_writer_pretty(file, &[prediction])
        .map_err(|error| format!("cannot serialize SWE-bench prediction: {error}"))
}
