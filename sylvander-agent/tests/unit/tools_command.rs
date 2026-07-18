use std::sync::{Arc, Mutex};

use sylvander_protocol::{AgentId, SessionContext, SessionId, UserId};

use super::*;

fn context(root: &std::path::Path) -> ToolContext {
    ToolContext::new(SessionContext::new(
        UserId::new("user"),
        AgentId::new("agent"),
        SessionId::new("session"),
    ))
    .with_fs_root(root)
    .with_capability(Cap::Spawn)
}

#[tokio::test]
async fn runs_in_effective_workspace_and_reports_status() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hello.txt"), "hello\n").unwrap();
    let output = CommandTool::new()
        .execute(&context(dir.path()), json!({"command": "cat hello.txt"}))
        .await
        .unwrap();
    assert!(!output.is_error);
    let result: JsonValue = serde_json::from_str(&output.content).unwrap();
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["stdout"], "hello\n");
    assert_eq!(result["stdout_truncated"], false);
}

#[tokio::test]
async fn non_zero_exit_is_visible_to_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let output = CommandTool::new()
        .execute(
            &context(dir.path()),
            json!({"command": "printf boom >&2; exit 7"}),
        )
        .await
        .unwrap();
    assert!(output.is_error);
    let result: JsonValue = serde_json::from_str(&output.content).unwrap();
    assert_eq!(result["exit_code"], 7);
    assert_eq!(result["stderr"], "boom");
    assert_eq!(result["stderr_truncated"], false);
}

#[tokio::test]
async fn streaming_keeps_recent_unicode_progress_without_returning_the_full_log() {
    let dir = tempfile::tempdir().unwrap();
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let captured = deltas.clone();
    let progress = ToolProgressSink::new(move |delta| captured.lock().unwrap().push(delta));

    let output = CommandTool::new()
        .execute_streaming(
            &context(dir.path()),
            json!({
                "command": "printf '开始\\n'; sleep 0.2; printf '完成\\n'; printf '警告\\n' >&2"
            }),
            progress,
        )
        .await
        .unwrap();

    assert!(!output.is_error);
    let progress = deltas.lock().unwrap().join("");
    assert!(progress.contains("开始"));
    assert!(progress.contains("完成"));
    assert!(progress.contains("[stderr] 警告"));
    assert!(progress.len() < output.content.len());
}
