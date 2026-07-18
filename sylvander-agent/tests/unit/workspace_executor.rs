use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::tool::Tool;
use crate::tool_context::{Cap, ToolContext};
use crate::tools::{CommandTool, EditTool, ReadTool, WriteTool};

fn context(target: WorkspaceTarget) -> ToolContext {
    ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_executor(Arc::new(LocalExecutor), target)
        .with_capability(Cap::Read)
        .with_capability(Cap::Write)
        .with_capability(Cap::Spawn)
}

async fn assert_executor_core_conformance(
    executor: &dyn WorkspaceExecutor,
    target: &WorkspaceTarget,
) {
    executor
        .write_file(target, "contract/data.txt", b"alpha\nneedle\nomega")
        .await
        .unwrap();
    let bounded = executor
        .read_file_bounded(target, "contract/data.txt", 5)
        .await
        .unwrap();
    assert_eq!(bounded.bytes, b"alpha");
    assert_eq!(bounded.total_bytes, 18);
    assert!(bounded.truncated);

    let listed = executor
        .list(
            target,
            WorkspaceListRequest {
                relative_path: "contract".into(),
                recursive: true,
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .unwrap();
    assert!(
        listed
            .entries
            .iter()
            .any(|entry| entry.relative_path.ends_with("contract/data.txt"))
    );
    let found = executor
        .search(
            target,
            WorkspaceSearchRequest {
                relative_path: "contract".into(),
                query: "needle".into(),
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(found.matches.len(), 1);

    let environment = BTreeMap::from([("SYLVANDER_CONFORMANCE".into(), "ready".into())]);
    let command = executor
        .run_command_with_environment(
            target,
            "printf %s \"$SYLVANDER_CONFORMANCE\"",
            Duration::from_secs(2),
            &environment,
        )
        .await
        .unwrap();
    assert_eq!(command.stdout, b"ready");

    let progress = Arc::new(Mutex::new(String::new()));
    let captured = progress.clone();
    executor
        .run_command_streaming_with_environment(
            target,
            "printf streamed",
            Duration::from_secs(2),
            &BTreeMap::new(),
            WorkspaceCommandProgressSink::new(move |_, delta| {
                captured.lock().unwrap().push_str(&delta);
            }),
        )
        .await
        .unwrap();
    assert_eq!(&*progress.lock().unwrap(), "streamed");

    let inspected = executor
        .run_read_only_command(target, "printf inspected", Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(inspected.stdout, b"inspected");
}

#[derive(Debug, Default)]
struct RecordingExecutor {
    reads: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl WorkspaceExecutor for RecordingExecutor {
    async fn read_file(
        &self,
        target: &WorkspaceTarget,
        relative_path: &str,
    ) -> Result<Vec<u8>, WorkspaceExecutorError> {
        self.reads
            .lock()
            .unwrap()
            .push((target.id.clone(), relative_path.into()));
        Ok(b"from-mock".to_vec())
    }

    async fn write_file(
        &self,
        _target: &WorkspaceTarget,
        _relative_path: &str,
        _content: &[u8],
    ) -> Result<(), WorkspaceExecutorError> {
        unreachable!("read contract does not write")
    }

    async fn run_command(
        &self,
        _target: &WorkspaceTarget,
        _command: &str,
        _timeout: Duration,
    ) -> Result<WorkspaceCommandOutput, WorkspaceExecutorError> {
        unreachable!("read contract does not spawn")
    }
}

#[tokio::test]
async fn tool_uses_injected_executor_and_preserves_target_identity() {
    let executor = Arc::new(RecordingExecutor::default());
    let context = ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_executor(
            executor.clone(),
            WorkspaceTarget {
                id: "container:dev".into(),
                workspace_path: "/workspace".into(),
                read_only: false,
            },
        )
        .with_capability(Cap::Read);
    let output = ReadTool::new("/must-not-be-used")
        .execute(&context, json!({"file_path":"src/lib.rs"}))
        .await
        .unwrap();
    assert_eq!(output.content, "from-mock");
    assert_eq!(
        *executor.reads.lock().unwrap(),
        [("container:dev".into(), "src/lib.rs".into())]
    );
}

#[tokio::test]
async fn local_executor_contract_covers_read_write_edit_and_command() {
    let workspace = tempfile::tempdir().unwrap();
    let context = context(WorkspaceTarget::local(workspace.path(), false));
    WriteTool::new("/")
        .execute(
            &context,
            json!({"file_path":"value.txt","content":"before"}),
        )
        .await
        .unwrap();
    EditTool::new("/")
        .execute(
            &context,
            json!({"file_path":"value.txt","old_string":"before","new_string":"after"}),
        )
        .await
        .unwrap();
    let read = ReadTool::new("/")
        .execute(&context, json!({"file_path":"value.txt"}))
        .await
        .unwrap();
    assert_eq!(read.content, "after");
    let command = CommandTool::new("/")
        .execute(&context, json!({"command":"printf command-ok"}))
        .await
        .unwrap();
    assert!(command.content.contains("command-ok"));
    assert_executor_core_conformance(
        &LocalExecutor,
        &WorkspaceTarget::local(workspace.path(), false),
    )
    .await;
}

#[tokio::test]
async fn local_command_environment_is_scoped_validated_and_streaming_compatible() {
    let workspace = tempfile::tempdir().unwrap();
    let context = context(WorkspaceTarget::local(workspace.path(), false));
    let output = CommandTool::new("/")
        .execute(
            &context,
            json!({
                "command":"printf %s \"$SYLVANDER_CONTRACT_VALUE\"",
                "environment":{"SYLVANDER_CONTRACT_VALUE":"蟹伙伴"}
            }),
        )
        .await
        .unwrap();
    assert!(!output.is_error);
    assert!(output.content.contains("蟹伙伴"));

    let invalid = CommandTool::new("/")
        .execute(
            &context,
            json!({
                "command":"printf unreachable",
                "environment":{"1_INVALID":"value"}
            }),
        )
        .await
        .unwrap();
    assert!(invalid.is_error);
    assert!(invalid.content.contains("invalid name or value"));
}

#[tokio::test]
async fn local_bounded_read_reports_total_and_retains_only_the_limit() {
    let workspace = tempfile::tempdir().unwrap();
    tokio::fs::write(workspace.path().join("value.txt"), b"abcdefgh")
        .await
        .unwrap();
    let target = WorkspaceTarget::local(workspace.path(), true);

    let read = LocalExecutor
        .read_file_bounded(&target, "value.txt", 4)
        .await
        .unwrap();

    assert_eq!(read.bytes, b"abcd");
    assert_eq!(read.total_bytes, 8);
    assert!(read.truncated);
    assert_eq!(
        LocalExecutor.read_file(&target, "value.txt").await.unwrap(),
        b"abcdefgh"
    );
}

#[tokio::test]
async fn local_command_drains_and_bounds_stdout_and_stderr_without_deadlock() {
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
    assert!(output.stdout_truncated);
    assert!(output.stderr_truncated);
    assert!(output.stdout.starts_with(b"HEAD"));
    assert!(output.stdout.ends_with(b"TAIL"));
    assert!(output.stderr.starts_with(b"HEAD"));
    assert!(output.stderr.ends_with(b"TAIL"));
}

#[tokio::test]
async fn local_command_timeout_cancels_the_process_group() {
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
    assert!(
        !survived.exists(),
        "timed-out command left a descendant process running"
    );
}

#[tokio::test]
async fn dropping_local_command_future_terminates_the_process_group() {
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
    assert!(ready.exists(), "command never reached its ready boundary");

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(
        !survived.exists(),
        "cancelled command left a descendant process running"
    );
}

#[test]
fn command_progress_preserves_utf8_split_across_reader_chunks() {
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
async fn read_only_target_rejects_every_mutating_tool() {
    let workspace = tempfile::tempdir().unwrap();
    tokio::fs::write(workspace.path().join("value.txt"), "before")
        .await
        .unwrap();
    let context = context(WorkspaceTarget::local(workspace.path(), true));
    let write = WriteTool::new("/")
        .execute(&context, json!({"file_path":"new.txt","content":"x"}))
        .await
        .unwrap();
    let edit = EditTool::new("/")
        .execute(
            &context,
            json!({"file_path":"value.txt","old_string":"before","new_string":"after"}),
        )
        .await
        .unwrap();
    let command = CommandTool::new("/")
        .execute(&context, json!({"command":"touch escaped"}))
        .await
        .unwrap();
    assert!(write.is_error && write.content.contains("read-only"));
    assert!(edit.is_error && edit.content.contains("read-only"));
    assert!(command.is_error && command.content.contains("read-only"));
    assert!(!workspace.path().join("new.txt").exists());
    assert!(!workspace.path().join("escaped").exists());
}

#[tokio::test]
async fn local_list_and_search_are_bounded_unicode_safe_and_read_only() {
    let workspace = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(workspace.path().join("子目录"))
        .await
        .unwrap();
    tokio::fs::write(
        workspace.path().join("子目录/说明.txt"),
        "第一行\n匹配：螃蟹伙伴很可靠\n匹配：第二处\n",
    )
    .await
    .unwrap();
    let target = WorkspaceTarget::local(workspace.path(), true);
    let executor = LocalExecutor;

    let list = executor
        .list(
            &target,
            WorkspaceListRequest {
                relative_path: ".".into(),
                recursive: true,
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .unwrap();
    assert!(
        list.entries
            .iter()
            .any(|entry| entry.relative_path == "子目录/说明.txt")
    );

    let search = executor
        .search(
            &target,
            WorkspaceSearchRequest {
                relative_path: ".".into(),
                query: "匹配".into(),
                limits: WorkspaceQueryLimits {
                    max_results: 1,
                    max_line_chars: 6,
                    ..WorkspaceQueryLimits::default()
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(search.matches.len(), 1);
    assert_eq!(search.matches[0].relative_path, "子目录/说明.txt");
    assert_eq!(search.matches[0].line, "匹配：螃蟹伙…");
    assert!(search.truncated);

    let output = executor
        .run_read_only_command(&target, "printf readonly-ok", Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(output.stdout, b"readonly-ok");
}

#[tokio::test]
async fn large_local_workspace_queries_stop_at_their_result_budget() {
    let workspace = tempfile::tempdir().unwrap();
    for directory in 0..25 {
        let directory = workspace.path().join(format!("src-{directory:02}"));
        std::fs::create_dir(&directory).unwrap();
        for file in 0..100 {
            std::fs::write(
                directory.join(format!("file-{file:03}.txt")),
                "performance-needle\n",
            )
            .unwrap();
        }
    }
    let target = WorkspaceTarget::local(workspace.path(), true);
    let executor = LocalExecutor;

    let listed = executor
        .list(
            &target,
            WorkspaceListRequest {
                relative_path: ".".into(),
                recursive: true,
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(listed.entries.len(), 200);
    assert!(listed.truncated);

    let searched = executor
        .search(
            &target,
            WorkspaceSearchRequest {
                relative_path: ".".into(),
                query: "performance-needle".into(),
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(searched.matches.len(), 200);
    assert!(searched.truncated);
}

#[tokio::test]
async fn local_query_limits_are_clamped_and_zero_is_rejected() {
    let bounded = WorkspaceQueryLimits {
        max_results: usize::MAX,
        max_line_chars: usize::MAX,
        max_output_bytes: usize::MAX,
        timeout: Duration::MAX,
    }
    .bounded()
    .unwrap();
    assert_eq!(bounded.max_results, MAX_QUERY_RESULTS);
    assert_eq!(bounded.max_line_chars, MAX_QUERY_LINE_CHARS);
    assert_eq!(bounded.max_output_bytes, MAX_QUERY_OUTPUT_BYTES);
    assert_eq!(bounded.timeout, MAX_QUERY_TIMEOUT);
    assert!(
        WorkspaceQueryLimits {
            max_results: 0,
            ..WorkspaceQueryLimits::default()
        }
        .bounded()
        .is_err()
    );
}

#[tokio::test]
async fn workspace_router_resolves_logical_mounts_and_enforces_capabilities() {
    let task = tempfile::tempdir().unwrap();
    let dependency = tempfile::tempdir().unwrap();
    let artifacts = tempfile::tempdir().unwrap();
    tokio::fs::write(task.path().join("task.txt"), "task")
        .await
        .unwrap();
    tokio::fs::write(dependency.path().join("lib.txt"), "dependency")
        .await
        .unwrap();
    let local = Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>;
    let mount = |path: &Path,
                 read_only: bool,
                 capabilities: sylvander_protocol::WorkspaceCapabilityPolicy| {
        MountedWorkspace {
            executor: local.clone(),
            target: WorkspaceTarget::local(path, read_only),
            capabilities,
        }
    };
    let router = WorkspaceRouter::new(
        "task",
        [
            (
                "task".into(),
                mount(
                    task.path(),
                    false,
                    sylvander_protocol::WorkspaceCapabilityPolicy {
                        read: true,
                        write: true,
                        command: true,
                        git: true,
                    },
                ),
            ),
            (
                "dependency".into(),
                mount(
                    dependency.path(),
                    true,
                    sylvander_protocol::WorkspaceCapabilityPolicy {
                        read: true,
                        git: true,
                        ..Default::default()
                    },
                ),
            ),
            (
                "artifacts".into(),
                mount(
                    artifacts.path(),
                    false,
                    sylvander_protocol::WorkspaceCapabilityPolicy {
                        read: true,
                        write: true,
                        ..Default::default()
                    },
                ),
            ),
        ],
    )
    .unwrap();
    let target = WorkspaceTarget::local("/", false);

    assert_eq!(
        router.read_file(&target, "task.txt").await.unwrap(),
        b"task"
    );
    assert_eq!(
        router
            .read_file(&target, "@dependency/lib.txt")
            .await
            .unwrap(),
        b"dependency"
    );
    router
        .write_file(&target, "@artifacts/report.txt", b"report")
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(artifacts.path().join("report.txt"))
            .await
            .unwrap(),
        b"report"
    );
    assert!(
        router
            .write_file(&target, "@dependency/forbidden.txt", b"x")
            .await
            .is_err()
    );
    let listing = router
        .list(
            &target,
            WorkspaceListRequest {
                relative_path: "@dependency/.".into(),
                recursive: false,
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .unwrap();
    assert!(
        listing
            .entries
            .iter()
            .any(|entry| entry.relative_path == "@dependency/lib.txt")
    );
    let dependency_target = router
        .select_mount_target(&target, Some("dependency"))
        .unwrap();
    assert!(
        router
            .run_command(
                &dependency_target,
                "printf forbidden",
                Duration::from_secs(1)
            )
            .await
            .is_err()
    );
    assert!(
        router
            .select_mount_target(&target, Some("missing"))
            .is_err()
    );
    let task_target = router.select_mount_target(&target, Some("task")).unwrap();
    assert_executor_core_conformance(&router, &task_target).await;
}

#[tokio::test]
async fn unavailable_target_never_falls_back_to_local() {
    let workspace = tempfile::tempdir().unwrap();
    tokio::fs::write(workspace.path().join("value.txt"), "secret")
        .await
        .unwrap();
    let context = ToolContext::new(sylvander_protocol::SessionContext::new("u", "a", "s"))
        .with_execution_target("ssh:build", workspace.path(), false)
        .with_capability(Cap::Read);
    let output = ReadTool::new(workspace.path())
        .execute(&context, json!({"file_path":"value.txt"}))
        .await
        .unwrap();
    assert!(output.is_error);
    assert!(output.content.contains("ssh:build"));
    assert!(output.content.contains("unavailable"));
}
