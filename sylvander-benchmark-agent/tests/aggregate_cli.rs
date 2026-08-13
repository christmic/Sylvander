use std::fs;
use std::process::Command;

use serde_json::json;
use tempfile::tempdir;

#[test]
fn aggregate_emits_one_deployment_summary() {
    let directory = tempdir().unwrap();
    let results = directory.path().join("results.jsonl");
    let value = json!({
        "schema_version": 1,
        "benchmark_id": "harbor",
        "dataset_name": "terminal-bench",
        "dataset_version": "2.0",
        "task_id": "task-a",
        "agent_revision": "revision",
        "provider_id": "provider",
        "protocol": "openai_chat_completions",
        "model_id": "model",
        "run_ordinal": 1,
        "status": "passed",
        "reward": 1.0,
        "sylvander_commit": "commit",
        "worktree_dirty": false,
        "harness_revision": "harbor-revision",
        "duration_ms": 100,
        "iterations": 1,
        "tool_calls": 2,
        "input_tokens": 10,
        "output_tokens": 5,
        "cached_tokens": 3,
        "failure_kind": null
    });
    fs::write(&results, format!("{value}\n")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sylvander-agent-bench"))
        .args(["aggregate", results.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let aggregate: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(aggregate["total_cells"], 1);
    assert_eq!(aggregate["executed_cells"], 1);
    assert_eq!(aggregate["pass_rate"], 1.0);
    assert_eq!(aggregate["total_input_tokens"], 10);
}
