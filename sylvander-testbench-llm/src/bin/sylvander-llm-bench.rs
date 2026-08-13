//! Command-line entry point for reviewing and executing LLM bench matrices.

use std::env;
use std::fs::File;
use std::io::{BufReader, Write as _};
use std::path::Path;

use sylvander_testbench_llm::BenchMatrix;

fn main() {
    if let Err(message) = run(env::args().skip(1).collect()) {
        eprintln!("sylvander-llm-bench: {message}");
        std::process::exit(2);
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let [command, matrix_path] = arguments.as_slice() else {
        return Err("usage: sylvander-llm-bench plan <matrix.json>".into());
    };
    if command != "plan" {
        return Err(format!("unsupported command {command}; expected plan"));
    }
    let matrix = read_matrix(Path::new(matrix_path))?;
    let cells = matrix.expand().map_err(str::to_owned)?;
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    for cell in &cells {
        serde_json::to_writer(&mut output, cell)
            .map_err(|error| format!("cannot serialize matrix cell: {error}"))?;
        output
            .write_all(b"\n")
            .map_err(|error| format!("cannot write matrix plan: {error}"))?;
    }
    eprintln!("planned {} matrix cells", cells.len());
    Ok(())
}

fn read_matrix(path: &Path) -> Result<BenchMatrix, String> {
    let file = File::open(path)
        .map_err(|error| format!("cannot open matrix {}: {error}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .map_err(|error| format!("cannot parse matrix {}: {error}", path.display()))
}
