//! Command-line review surface for external Agent benchmark matrices.

use std::env;
use std::fs::File;
use std::io::{BufReader, Write as _};
use std::path::Path;

use serde::de::DeserializeOwned;
use sylvander_benchmark_agent::Trajectory;
use sylvander_benchmark_agent::harbor_result::{HarborTrialResult, normalize_harbor_result};
use sylvander_benchmark_agent::matrix::AgentBenchMatrix;
use sylvander_benchmark_agent::matrix::AgentMatrixCoordinate;
use sylvander_benchmark_agent::result::{AgentBenchStatus, RepositoryState};

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("sylvander-agent-bench: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    match arguments.as_slice() {
        [command, matrix_path] if command == "plan" => plan(Path::new(matrix_path)),
        [command, coordinate_path, trial_path, trajectory_path, harness_revision]
            if command == "ingest" =>
        {
            ingest(
                Path::new(coordinate_path),
                Path::new(trial_path),
                Path::new(trajectory_path),
                harness_revision,
            )
        }
        _ => Err(
            "usage: sylvander-agent-bench plan <matrix.json> | ingest <coordinate.json> <trial-result.json> <trajectory.json> <harness-revision>".into(),
        ),
    }
}

fn plan(matrix_path: &Path) -> Result<(), String> {
    let matrix = read_matrix(matrix_path)?;
    let cells = matrix.expand().map_err(str::to_owned)?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for cell in &cells {
        serde_json::to_writer(&mut output, cell)
            .map_err(|error| format!("cannot serialize Agent matrix cell: {error}"))?;
        output
            .write_all(b"\n")
            .map_err(|error| format!("cannot write Agent matrix plan: {error}"))?;
    }
    eprintln!("planned {} Agent matrix cells", cells.len());
    Ok(())
}

fn ingest(
    coordinate_path: &Path,
    trial_path: &Path,
    trajectory_path: &Path,
    harness_revision: &str,
) -> Result<(), String> {
    let coordinate = read_json::<AgentMatrixCoordinate>(coordinate_path)?;
    let trial = read_json::<HarborTrialResult>(trial_path)?;
    let trajectory = read_json::<Trajectory>(trajectory_path)?;
    let result = normalize_harbor_result(
        coordinate,
        RepositoryState::discover(),
        harness_revision,
        &trial,
        &trajectory,
    )
    .map_err(|error| error.to_string())?;
    serde_json::to_writer(std::io::stdout().lock(), &result)
        .map_err(|error| format!("cannot serialize Agent benchmark result: {error}"))?;
    println!();
    if result.status != AgentBenchStatus::Passed {
        return Err(format!(
            "Agent benchmark result is not passing: {:?}",
            result.status
        ));
    }
    Ok(())
}

fn read_matrix(path: &Path) -> Result<AgentBenchMatrix, String> {
    read_json(path)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, String> {
    let file = File::open(path).map_err(|error| {
        format!(
            "cannot open Agent benchmark input {}: {error}",
            path.display()
        )
    })?;
    serde_json::from_reader(BufReader::new(file)).map_err(|error| {
        format!(
            "cannot parse Agent benchmark input {}: {error}",
            path.display()
        )
    })
}
