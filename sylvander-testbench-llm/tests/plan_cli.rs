use std::fs;
use std::process::Command;

use serde_json::Value;

#[test]
fn plan_outputs_one_content_safe_json_line_per_matrix_cell() {
    let matrix_path =
        std::env::temp_dir().join(format!("sylvander-llm-matrix-{}.json", std::process::id()));
    fs::write(
        &matrix_path,
        r#"{
          "schema_version": 1,
          "repetitions": 2,
          "scenarios": ["connectivity"],
          "bindings": [{
            "provider_id": "provider-a",
            "protocol": "openai_responses",
            "base_url": "https://api.example.test/v1",
            "credential_env": "SECRET_PROVIDER_A_KEY",
            "supported_scenarios": ["connectivity"],
            "models": [{
              "model_id": "model-a",
              "advertised_scenarios": ["connectivity"]
            }]
          }]
        }"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_sylvander-llm-bench"))
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
    assert_eq!(values[0]["coordinate"]["provider_id"], "provider-a");
    assert!(!lines.contains("SECRET_PROVIDER_A_KEY"));
    assert!(!lines.contains("api.example.test"));
}
