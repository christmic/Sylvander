use std::time::Duration;

use super::*;

#[tokio::test]
async fn seatbelt_allows_workspace_writes_and_denies_outside_writes() {
    assert!(MacosSeatbeltExecutor::available());
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = WorkspaceTarget::local(workspace.path(), false);
    let executor = MacosSeatbeltExecutor;

    let inside = executor
        .run_command(
            &target,
            "printf inside > result.txt",
            Duration::from_secs(5),
        )
        .await
        .unwrap();
    assert!(
        inside.success,
        "{}",
        String::from_utf8_lossy(&inside.stderr)
    );
    assert_eq!(
        tokio::fs::read(workspace.path().join("result.txt"))
            .await
            .unwrap(),
        b"inside"
    );

    let command = format!("printf escaped > {}/escaped.txt", outside.path().display());
    let escaped = executor
        .run_command(&target, &command, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(!escaped.success);
    assert!(!outside.path().join("escaped.txt").exists());

    let isolation = executor.process_isolation();
    assert!(isolation.enforces_process_sandbox());
    assert!(!isolation.enforces_resource_limits());
}
