use std::fs;
use std::process::Command;

use serde_json::json;
use tempfile::tempdir;

#[test]
fn ingest_emits_normalized_passing_evidence() {
    let directory = tempdir().unwrap();
    let coordinate = directory.path().join("coordinate.json");
    let trial = directory.path().join("result.json");
    let trajectory = directory.path().join("trajectory.json");
    fs::write(
        &coordinate,
        serde_json::to_vec(&json!({
            "benchmark_id": "harbor",
            "dataset_name": "terminal-bench",
            "dataset_version": "2.0",
            "task_id": "task-a",
            "agent_revision": "revision",
            "provider_id": "provider",
            "protocol": "openai_chat_completions",
            "model_id": "model",
            "run_ordinal": 1
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        &trial,
        serde_json::to_vec(&json!({
            "task_name": "task-a",
            "agent_info": {
                "name": "sylvander",
                "model_info": {"name": "model", "provider": "provider"}
            },
            "agent_result": {
                "n_input_tokens": 20,
                "n_cache_tokens": 5,
                "n_output_tokens": 10
            },
            "verifier_result": {"rewards": {"reward": 1}},
            "exception_info": null,
            "started_at": "2026-08-13T00:00:00Z",
            "finished_at": "2026-08-13T00:00:01Z"
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(
        &trajectory,
        serde_json::to_vec(&json!({
            "schema_version": "ATIF-v1.7",
            "agent": {
                "name": "sylvander",
                "version": "0.1.0",
                "model_name": "provider/model"
            },
            "steps": [{
                "step_id": 1,
                "source": "agent",
                "model_name": "provider/model",
                "message": "done",
                "llm_call_count": 1
            }],
            "final_metrics": {
                "total_prompt_tokens": 20,
                "total_completion_tokens": 10,
                "total_cached_tokens": 5,
                "total_steps": 1
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sylvander-agent-bench"))
        .args([
            "ingest",
            coordinate.to_str().unwrap(),
            trial.to_str().unwrap(),
            trajectory.to_str().unwrap(),
            "harbor-ea2fee7",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(result["reward"], 1.0);
    assert_eq!(result["duration_ms"], 1_000);
    assert!(result.get("instruction").is_none());
}
