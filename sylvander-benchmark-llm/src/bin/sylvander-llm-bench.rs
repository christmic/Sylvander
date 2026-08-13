//! Command-line entry point for reviewing and executing LLM bench matrices.

use std::env;
use std::fs::File;
use std::io::{BufReader, Write as _};
use std::path::Path;
use std::time::Duration;

use sylvander_benchmark_llm::{
    BenchMatrix, BenchStatus, LiveLimits, ProtocolBinding, RepositoryState, run_crash_fixture,
    run_live_cell, run_process_interruption_cell,
};

#[tokio::main]
async fn main() {
    match run(env::args().skip(1).collect()).await {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(message) => {
            eprintln!("sylvander-llm-bench: {message}");
            std::process::exit(2);
        }
    }
}

async fn run(arguments: Vec<String>) -> Result<bool, String> {
    let [command, matrix_path] = arguments.as_slice() else {
        return Err("usage: sylvander-llm-bench <plan|run> <matrix.json>".into());
    };
    if command == "crash-fixture" {
        run_crash_fixture(Path::new(matrix_path)).await?;
        return Ok(true);
    }
    let matrix = read_matrix(Path::new(matrix_path))?;
    let cells = matrix.expand().map_err(str::to_owned)?;
    match command.as_str() {
        "plan" => emit_plan(&cells),
        "run" => execute(&matrix, &cells).await,
        _ => Err(format!(
            "unsupported command {command}; expected plan or run"
        )),
    }
}

fn emit_plan(cells: &[sylvander_benchmark_llm::MatrixCell]) -> Result<bool, String> {
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for cell in cells {
        serde_json::to_writer(&mut output, cell)
            .map_err(|error| format!("cannot serialize matrix cell: {error}"))?;
        output
            .write_all(b"\n")
            .map_err(|error| format!("cannot write matrix plan: {error}"))?;
    }
    eprintln!("planned {} matrix cells", cells.len());
    Ok(true)
}

async fn execute(
    matrix: &BenchMatrix,
    cells: &[sylvander_benchmark_llm::MatrixCell],
) -> Result<bool, String> {
    let repository = RepositoryState::discover();
    let limits = LiveLimits {
        request_timeout: Duration::from_millis(matrix.request_timeout_ms),
        max_output_tokens: matrix.max_output_tokens,
        max_retries: matrix.max_retries,
    };
    let mut accepted = true;
    for cell in cells {
        let binding = binding_for(matrix, cell)?;
        let result = if cell.coordinate.scenario
            == sylvander_benchmark_llm::BenchScenario::ProcessInterruption
        {
            run_process_interruption_cell(binding, cell, repository.clone()).await
        } else {
            run_live_cell(binding, cell, limits, repository.clone()).await
        };
        accepted &= matches!(
            result.status,
            BenchStatus::Passed | BenchStatus::NotApplicable
        );
        println!(
            "{}",
            serde_json::to_string(&result)
                .map_err(|error| format!("cannot serialize bench result: {error}"))?
        );
    }
    Ok(accepted)
}

fn binding_for<'a>(
    matrix: &'a BenchMatrix,
    cell: &sylvander_benchmark_llm::MatrixCell,
) -> Result<&'a ProtocolBinding, String> {
    matrix
        .bindings
        .iter()
        .find(|binding| {
            binding.provider_id == cell.coordinate.provider_id
                && binding.protocol == cell.coordinate.protocol
                && binding
                    .models
                    .iter()
                    .any(|model| model.model_id == cell.coordinate.model_id)
        })
        .ok_or_else(|| "expanded matrix cell lost its protocol binding".into())
}

fn read_matrix(path: &Path) -> Result<BenchMatrix, String> {
    let file = File::open(path)
        .map_err(|error| format!("cannot open matrix {}: {error}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .map_err(|error| format!("cannot parse matrix {}: {error}", path.display()))
}
