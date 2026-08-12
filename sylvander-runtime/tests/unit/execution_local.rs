use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

#[tokio::test]
async fn local_executor_contract_covers_bounded_files_queries_and_commands() {
    let workspace = tempfile::tempdir().unwrap();
    let target = WorkspaceTarget::local(workspace.path(), false);
    let executor = LocalExecutor;

    executor
        .write_file(&target, "nested/value.txt", b"alpha\nneedle\nomega")
        .await
        .unwrap();
    let read = executor
        .read_file_bounded(&target, "nested/value.txt", 5)
        .await
        .unwrap();
    assert_eq!(read.bytes, b"alpha");
    assert_eq!(read.total_bytes, 18);
    assert!(read.truncated);

    let listed = executor
        .list(
            &target,
            WorkspaceListRequest {
                relative_path: "nested".into(),
                recursive: true,
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(listed.entries.len(), 1);
    assert_eq!(listed.entries[0].relative_path, "nested/value.txt");

    let found = executor
        .search(
            &target,
            WorkspaceSearchRequest {
                relative_path: "nested".into(),
                query: "needle".into(),
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(found.matches.len(), 1);
    assert_eq!(found.matches[0].line_number, 2);

    let environment = BTreeMap::from([("SYLVANDER_LOCAL_TEST".into(), "ready".into())]);
    let command = executor
        .run_command_with_environment(
            &target,
            "printf %s \"$SYLVANDER_LOCAL_TEST\"",
            Duration::from_secs(2),
            &environment,
        )
        .await
        .unwrap();
    assert_eq!(command.stdout, b"ready");
    assert!(!executor.process_isolation().enforces_sandbox());
}

#[tokio::test]
async fn local_executor_rejects_read_only_mutation_and_path_escape() {
    let workspace = tempfile::tempdir().unwrap();
    tokio::fs::write(workspace.path().join("value.txt"), b"value")
        .await
        .unwrap();
    let target = WorkspaceTarget::local(workspace.path(), true);
    let executor = LocalExecutor;

    assert!(matches!(
        executor.write_file(&target, "new.txt", b"x").await,
        Err(WorkspaceExecutorError::ReadOnly(_))
    ));
    assert!(matches!(
        executor
            .run_command(&target, "touch escaped", Duration::from_secs(1))
            .await,
        Err(WorkspaceExecutorError::ReadOnly(_))
    ));
    assert!(matches!(
        executor.read_file(&target, "../value.txt").await,
        Err(WorkspaceExecutorError::InvalidPath(_))
    ));
    assert!(!workspace.path().join("new.txt").exists());
    assert!(!workspace.path().join("escaped").exists());
}

#[tokio::test]
async fn local_command_bounds_and_drains_stdout_and_stderr() {
    let workspace = tempfile::tempdir().unwrap();
    let target = WorkspaceTarget::local(workspace.path(), false);
    let payload_bytes = MAX_COMMAND_OUTPUT_BYTES_PER_STREAM + 8 * 1024;
    let expected_total = (payload_bytes + 8) as u64;
    let command = format!(
        "(printf HEAD; head -c {payload_bytes} /dev/zero | tr '\\\\000' o; printf TAIL) & \
         (printf HEAD >&2; head -c {payload_bytes} /dev/zero | tr '\\\\000' e >&2; \
         printf TAIL >&2) & wait"
    );

    let output = LocalExecutor
        .run_command(&target, &command, Duration::from_secs(5))
        .await
        .unwrap();

    assert!(output.success);
    assert_eq!(output.stdout.len(), MAX_COMMAND_OUTPUT_BYTES_PER_STREAM);
    assert_eq!(output.stderr.len(), MAX_COMMAND_OUTPUT_BYTES_PER_STREAM);
    assert_eq!(output.stdout_total_bytes, expected_total);
    assert_eq!(output.stderr_total_bytes, expected_total);
    assert!(output.stdout_truncated && output.stderr_truncated);
    assert!(output.stdout.starts_with(b"HEAD") && output.stdout.ends_with(b"TAIL"));
    assert!(output.stderr.starts_with(b"HEAD") && output.stderr.ends_with(b"TAIL"));
}

#[tokio::test]
async fn local_command_timeout_terminates_the_process_tree() {
    let workspace = tempfile::tempdir().unwrap();
    let target = WorkspaceTarget::local(workspace.path(), false);
    let survived = workspace.path().join("survived");
    let timeout = Duration::from_millis(30);

    let result = LocalExecutor
        .run_command(
            &target,
            "(sleep 1; printf survived > survived) & wait",
            timeout,
        )
        .await;

    assert!(matches!(
        result,
        Err(WorkspaceExecutorError::Timeout(value)) if value == timeout
    ));
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(!survived.exists());
}

#[tokio::test]
async fn dropping_local_command_future_terminates_the_process_tree() {
    let workspace = tempfile::tempdir().unwrap();
    let target = WorkspaceTarget::local(workspace.path(), false);
    let ready = workspace.path().join("ready");
    let survived = workspace.path().join("survived");
    let task = tokio::spawn(async move {
        LocalExecutor
            .run_command(
                &target,
                "printf ready > ready; (sleep 1; printf survived > survived) & wait",
                Duration::from_secs(10),
            )
            .await
    });
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(ready.exists());

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(!survived.exists());
}

#[test]
fn local_progress_preserves_utf8_across_reader_chunks() {
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let captured = deltas.clone();
    let sink = WorkspaceCommandProgressSink::new(move |stream, delta| {
        captured.lock().unwrap().push((stream, delta));
    });
    let mut pending = Vec::new();
    let crab = "蟹".as_bytes();

    emit_utf8_progress(
        WorkspaceCommandStream::Stdout,
        &sink,
        &mut pending,
        &crab[..1],
        false,
    );
    emit_utf8_progress(
        WorkspaceCommandStream::Stdout,
        &sink,
        &mut pending,
        &crab[1..],
        true,
    );

    assert_eq!(
        *deltas.lock().unwrap(),
        [(WorkspaceCommandStream::Stdout, "蟹".into())]
    );
}

#[tokio::test]
async fn local_query_limits_are_bounded_and_empty_search_is_rejected() {
    let workspace = tempfile::tempdir().unwrap();
    tokio::fs::write(workspace.path().join("value.txt"), b"needle")
        .await
        .unwrap();
    let target = WorkspaceTarget::local(workspace.path(), true);

    let invalid = LocalExecutor
        .search(
            &target,
            WorkspaceSearchRequest {
                relative_path: ".".into(),
                query: String::new(),
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await;
    assert!(matches!(
        invalid,
        Err(WorkspaceExecutorError::InvalidRequest(_))
    ));
    assert!(
        WorkspaceQueryLimits {
            max_results: 0,
            ..WorkspaceQueryLimits::default()
        }
        .bounded()
        .is_err()
    );
}
