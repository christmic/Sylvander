//! Command-line review surface for external Agent benchmark matrices.

use std::env;
use std::fs::File;
use std::io::{BufReader, Write as _};
use std::path::Path;

use sylvander_benchmark_agent::matrix::AgentBenchMatrix;

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("sylvander-agent-bench: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let [command, matrix_path] = arguments.as_slice() else {
        return Err("usage: sylvander-agent-bench plan <matrix.json>".into());
    };
    if command != "plan" {
        return Err("only the non-billable plan command is currently supported".into());
    }
    let matrix = read_matrix(Path::new(matrix_path))?;
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

fn read_matrix(path: &Path) -> Result<AgentBenchMatrix, String> {
    let file = File::open(path)
        .map_err(|error| format!("cannot open Agent matrix {}: {error}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .map_err(|error| format!("cannot parse Agent matrix {}: {error}", path.display()))
}
