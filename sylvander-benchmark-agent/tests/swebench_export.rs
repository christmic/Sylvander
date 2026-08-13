use std::process::Command;

use sylvander_benchmark_agent::swebench::SweBenchPrediction;

fn git(workspace: &std::path::Path, arguments: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn exports_the_official_prediction_fields_from_a_workspace_diff() {
    let workspace = tempfile::tempdir().unwrap();
    git(workspace.path(), &["init", "-q"]);
    git(
        workspace.path(),
        &["config", "user.name", "Sylvander Bench"],
    );
    git(
        workspace.path(),
        &["config", "user.email", "bench@example.test"],
    );
    std::fs::write(workspace.path().join("answer.txt"), "before\n").unwrap();
    git(workspace.path(), &["add", "answer.txt"]);
    git(workspace.path(), &["commit", "-qm", "fixture"]);
    std::fs::write(workspace.path().join("answer.txt"), "after\n").unwrap();

    let prediction = SweBenchPrediction::from_workspace(
        workspace.path(),
        "project__repo-1",
        "sylvander/minimax/MiniMax-M2.7",
    )
    .unwrap();
    let value = serde_json::to_value(prediction).unwrap();

    assert_eq!(value["instance_id"], "project__repo-1");
    assert_eq!(
        value["model_name_or_path"],
        "sylvander/minimax/MiniMax-M2.7"
    );
    assert!(value["model_patch"].as_str().unwrap().contains("+after"));
}

#[test]
fn refuses_an_empty_patch() {
    let workspace = tempfile::tempdir().unwrap();
    git(workspace.path(), &["init", "-q"]);
    git(
        workspace.path(),
        &["config", "user.name", "Sylvander Bench"],
    );
    git(
        workspace.path(),
        &["config", "user.email", "bench@example.test"],
    );
    std::fs::write(workspace.path().join("answer.txt"), "same\n").unwrap();
    git(workspace.path(), &["add", "answer.txt"]);
    git(workspace.path(), &["commit", "-qm", "fixture"]);

    assert!(SweBenchPrediction::from_workspace(workspace.path(), "instance", "model").is_err());
}
