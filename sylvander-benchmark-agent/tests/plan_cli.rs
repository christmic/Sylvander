use std::fs;
use std::process::Command;

use serde_json::Value;

#[test]
fn plan_outputs_the_complete_external_coordinate_without_execution() {
    let matrix_path = std::env::temp_dir().join(format!(
        "sylvander-agent-matrix-{}.json",
        std::process::id()
    ));
    fs::write(
        &matrix_path,
        r#"{
          "schema_version": 1,
          "repetitions": 2,
          "benchmarks": [{
            "benchmark_id": "harbor",
            "dataset_name": "terminal-bench",
            "dataset_version": "2.0",
            "tasks": [{
              "task_id": "task-a",
              "required_capabilities": ["terminal"]
            }]
          }],
          "deployments": [{
            "agent_revision": "agent-r1",
            "provider_id": "minimax",
            "protocol": "openai_chat_completions",
            "model_id": "MiniMax-M2.7",
            "capabilities": ["terminal"]
          }]
        }"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sylvander-agent-bench"))
        .args(["plan", matrix_path.to_str().unwrap()])
        .output()
        .unwrap();
    fs::remove_file(matrix_path).unwrap();

    assert!(output.status.success());
    let lines = String::from_utf8(output.stdout).unwrap();
    let values = lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["coordinate"]["dataset_version"], "2.0");
    assert_eq!(values[1]["coordinate"]["run_ordinal"], 2);
}
