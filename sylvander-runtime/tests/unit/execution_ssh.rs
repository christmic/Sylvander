use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::*;

struct FakeSsh {
    _directory: TempDir,
    executable: PathBuf,
    argv_log: PathBuf,
    stdin_log: PathBuf,
}

impl FakeSsh {
    fn new(body: &str) -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let executable = directory.path().join("fake-ssh");
        let argv_log = directory.path().join("argv");
        let stdin_log = directory.path().join("fake-ssh.stdin");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n{}",
            shell_quote(argv_log.to_str().expect("UTF-8 temp path")),
            body
        );
        fs::write(&executable, script).expect("write fake ssh");
        let mut permissions = fs::metadata(&executable).expect("metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions).expect("chmod");
        Self {
            _directory: directory,
            executable,
            argv_log,
            stdin_log,
        }
    }

    fn executor(&self) -> SshExecutor {
        SshExecutor::with_executable(
            &self.executable,
            "dev.example",
            2222,
            "agent-user",
            "/keys/id test",
            "/etc/ssh/sylvander-known-hosts",
            "/tmp/sylvander-ssh-%C",
        )
        .expect("valid executor")
    }
}

fn target(read_only: bool) -> WorkspaceTarget {
    WorkspaceTarget {
        id: "remote-dev".into(),
        workspace_path: PathBuf::from("/srv/工作区/it's safe"),
        read_only,
    }
}

#[tokio::test]
async fn read_uses_fixed_batch_argv_and_preserves_unicode() {
    let fake = FakeSsh::new("printf '你好，Sylvander'");
    let bytes = fake
        .executor()
        .read_file(&target(false), "文档/计划.md")
        .await
        .expect("read succeeds");
    assert_eq!(String::from_utf8(bytes).expect("UTF-8"), "你好，Sylvander");

    let argv = fs::read_to_string(&fake.argv_log).expect("argv log");
    assert!(argv.contains("-o\nBatchMode=yes"));
    assert!(argv.contains("-o\nStrictHostKeyChecking=yes"));
    assert!(argv.contains("-o\nUserKnownHostsFile=/etc/ssh/sylvander-known-hosts"));
    assert!(argv.contains("-o\nControlMaster=auto"));
    assert!(argv.contains("-o\nControlPersist=60"));
    assert!(argv.contains("-o\nControlPath=/tmp/sylvander-ssh-%C"));
    assert!(argv.contains("-p\n2222\n-i\n/keys/id test"));
    assert!(argv.contains("agent-user@dev.example"));
    assert!(argv.contains("exec cat -- \"$resolved\""));
    assert!(argv.contains("'/srv/工作区/it'\\''s safe'"));
    assert!(argv.contains("'文档/计划.md'"));
}

#[tokio::test]
async fn bounded_read_transfers_only_the_limit_probe_and_reports_total_bytes() {
    let fake = FakeSsh::new("printf '12\\0hello world!'");
    let result = fake
        .executor()
        .read_file_bounded(&target(false), "文档/计划.md", 5)
        .await
        .expect("bounded read succeeds");
    assert_eq!(result.bytes, b"hello");
    assert_eq!(result.total_bytes, 12);
    assert!(result.truncated);

    let argv = fs::read_to_string(&fake.argv_log).expect("argv log");
    assert!(argv.contains("exec head -c \"$3\" \"$resolved\""));
    assert!(argv.contains("'文档/计划.md'"));
    assert!(argv.contains("'6'"));
}

#[tokio::test]
async fn write_sends_content_only_on_stdin() {
    let fake = FakeSsh::new("cat > \"$0.stdin\"");
    let content = "line one\n'$(touch should-not-run)'\n中文".as_bytes();
    fake.executor()
        .write_file(&target(false), "src/输入.txt", content)
        .await
        .expect("write succeeds");
    assert_eq!(fs::read(&fake.stdin_log).expect("stdin log"), content);
    let argv = fs::read_to_string(&fake.argv_log).expect("argv log");
    assert!(!argv.contains("touch should-not-run"));
}

#[tokio::test]
async fn command_reports_status_and_streams_program_on_stdin() {
    let fake = FakeSsh::new("cat > \"$0.stdin\"\nprintf out\nprintf err >&2\nexit 7");
    let output = fake
        .executor()
        .run_command(
            &target(false),
            "printf '%s' \"用户命令; $(safe as data)\"",
            Duration::from_secs(5),
        )
        .await
        .expect("command completes");
    assert!(!output.success);
    assert_eq!(output.status_code, Some(7));
    assert_eq!(output.stdout, b"out");
    assert_eq!(output.stderr, b"err");
    assert!(!output.stdout_truncated);
    assert!(!output.stderr_truncated);
    assert_eq!(output.stdout_total_bytes, 3);
    assert_eq!(output.stderr_total_bytes, 3);
    assert_eq!(
        fs::read_to_string(&fake.stdin_log).expect("stdin log"),
        "printf '%s' \"用户命令; $(safe as data)\""
    );
    let argv = fs::read_to_string(&fake.argv_log).expect("argv log");
    assert!(argv.contains("command -v setsid"));
    assert!(argv.contains("set -m 2>/dev/null"));
    assert!(argv.contains("kill -TERM -- \"-$child\""));
}

#[tokio::test]
async fn command_concurrently_drains_and_bounds_both_output_streams() {
    const BODY_BYTES: usize = 300_000;
    const STDOUT_HEAD: &str = "stdout-head\n";
    const STDOUT_TAIL: &str = "\nstdout-tail";
    const STDERR_HEAD: &str = "stderr-head\n";
    const STDERR_TAIL: &str = "\nstderr-tail";
    let fake = FakeSsh::new(&format!(
        "printf '{STDOUT_HEAD}'\n\
         awk 'BEGIN {{ for (i = 0; i < {BODY_BYTES}; i++) printf \"o\" }}'\n\
         printf '{STDOUT_TAIL}'\n\
         printf '{STDERR_HEAD}' >&2\n\
         awk 'BEGIN {{ for (i = 0; i < {BODY_BYTES}; i++) printf \"e\" }}' >&2\n\
         printf '{STDERR_TAIL}' >&2"
    ));
    let output = fake
        .executor()
        .run_command(&target(false), "true", Duration::from_secs(5))
        .await
        .expect("large dual-stream command completes");

    assert!(output.success);
    assert!(output.stdout_truncated);
    assert!(output.stderr_truncated);
    assert_eq!(output.stdout.len(), MAX_COMMAND_OUTPUT_BYTES_PER_STREAM);
    assert_eq!(output.stderr.len(), MAX_COMMAND_OUTPUT_BYTES_PER_STREAM);
    assert_eq!(
        output.stdout_total_bytes,
        (STDOUT_HEAD.len() + BODY_BYTES + STDOUT_TAIL.len()) as u64
    );
    assert_eq!(
        output.stderr_total_bytes,
        (STDERR_HEAD.len() + BODY_BYTES + STDERR_TAIL.len()) as u64
    );
    assert!(output.stdout.starts_with(STDOUT_HEAD.as_bytes()));
    assert!(output.stdout.ends_with(STDOUT_TAIL.as_bytes()));
    assert!(output.stderr.starts_with(STDERR_HEAD.as_bytes()));
    assert!(output.stderr.ends_with(STDERR_TAIL.as_bytes()));
}

#[tokio::test]
async fn command_streaming_preserves_unicode_and_stream_identity() {
    let fake = FakeSsh::new("printf '远程输出'; printf '远程错误' >&2");
    let deltas = Arc::new(Mutex::new(Vec::new()));
    let captured = deltas.clone();
    let progress = WorkspaceCommandProgressSink::new(move |stream, delta| {
        captured.lock().unwrap().push((stream, delta));
    });

    let output = fake
        .executor()
        .run_command_streaming(
            &target(false),
            "printf ignored",
            Duration::from_secs(5),
            progress,
        )
        .await
        .expect("streaming command completes");

    assert!(output.success);
    let deltas = deltas.lock().unwrap();
    assert!(
        deltas
            .iter()
            .any(|(stream, delta)| *stream == WorkspaceCommandStream::Stdout
                && delta.contains("远程输出"))
    );
    assert!(
        deltas
            .iter()
            .any(|(stream, delta)| *stream == WorkspaceCommandStream::Stderr
                && delta.contains("远程错误"))
    );
}

#[tokio::test]
async fn dropping_command_future_terminates_the_ssh_transport() {
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("ready");
    let survived = directory.path().join("survived");
    let fake = FakeSsh::new(&format!(
        "case \"$*\" in *attempt=0*) exit 0;; esac\n\
         printf ready > {}; sleep 1; printf survived > {}",
        shell_quote(ready.to_str().unwrap()),
        shell_quote(survived.to_str().unwrap())
    ));
    let executor = fake.executor();
    let task = tokio::spawn(async move {
        executor
            .run_command(&target(false), "ignored", Duration::from_secs(10))
            .await
    });
    for _ in 0..500 {
        if ready.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        ready.exists(),
        "SSH transport never reached its ready boundary"
    );

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    assert!(
        !survived.exists(),
        "cancelled SSH transport continued after its future was dropped"
    );
}

#[tokio::test]
async fn structured_list_and_search_parse_bounded_unicode_results() {
    let list_fake =
        FakeSsh::new("printf '%s\\0%s\\0%s\\0%s\\0%s\\0%s\\0' './src' d 0 './src/螃蟹.rs' f 12");
    let list = list_fake
        .executor()
        .list(
            &target(true),
            WorkspaceListRequest {
                relative_path: ".".into(),
                recursive: true,
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .expect("list succeeds on a read-only target");
    assert_eq!(list.entries.len(), 2);
    assert_eq!(list.entries[0].kind, WorkspaceEntryKind::Directory);
    assert_eq!(list.entries[1].relative_path, "src/螃蟹.rs");
    assert_eq!(list.entries[1].size, 12);
    assert!(!list.truncated);

    let search_fake = FakeSsh::new(
        "printf '%s\\0%s\\0%s\\0%s\\0%s\\0%s\\0' './src/螃蟹.rs' 7 '匹配：可靠伙伴' './src/螃蟹.rs' 9 '匹配：再次'",
    );
    let search = search_fake
        .executor()
        .search(
            &target(true),
            WorkspaceSearchRequest {
                relative_path: "src".into(),
                query: "匹配".into(),
                limits: WorkspaceQueryLimits {
                    max_results: 1,
                    max_line_chars: 5,
                    ..WorkspaceQueryLimits::default()
                },
            },
        )
        .await
        .expect("search succeeds on a read-only target");
    assert_eq!(search.matches.len(), 1);
    assert_eq!(search.matches[0].line_number, 7);
    assert_eq!(search.matches[0].line, "匹配：可靠…");
    assert!(search.truncated);
    let argv = fs::read_to_string(&search_fake.argv_log).expect("argv log");
    assert!(argv.contains("'src'"));
    assert!(argv.contains("'匹配'"));
}

#[cfg(unix)]
#[test]
fn structured_queries_reject_symlink_paths_outside_the_workspace() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    fs::write(outside.path().join("secret.txt"), "outside-secret\n").expect("outside file");
    symlink(outside.path(), workspace.path().join("escape")).expect("workspace symlink");

    let list = std::process::Command::new("sh")
        .args([
            "-c",
            LIST_SCRIPT,
            "--",
            workspace.path().to_str().expect("UTF-8 workspace"),
            "escape",
            "1",
            "4096",
        ])
        .output()
        .expect("run list script");
    assert_eq!(list.status.code(), Some(126));
    assert!(list.stdout.is_empty());

    let search = std::process::Command::new("sh")
        .args([
            "-c",
            SEARCH_SCRIPT,
            "--",
            workspace.path().to_str().expect("UTF-8 workspace"),
            "escape/secret.txt",
            "outside-secret",
            "4096",
        ])
        .output()
        .expect("run search script");
    assert_eq!(search.status.code(), Some(126));
    assert!(search.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn reads_reject_external_symlinks_and_bound_large_files_remotely() {
    use std::os::unix::fs::symlink;

    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside");
    fs::write(outside.path().join("secret.txt"), "outside-secret\n").expect("outside file");
    symlink(outside.path(), workspace.path().join("escape")).expect("workspace symlink");

    for script in [READ_SCRIPT, READ_BOUNDED_SCRIPT] {
        let mut arguments = vec![
            "-c",
            script,
            "--",
            workspace.path().to_str().expect("UTF-8 workspace"),
            "escape/secret.txt",
        ];
        if script == READ_BOUNDED_SCRIPT {
            arguments.push("17");
        }
        let output = std::process::Command::new("sh")
            .args(arguments)
            .output()
            .expect("run read script");
        assert_eq!(output.status.code(), Some(126));
        assert!(output.stdout.is_empty());
    }

    let content = vec![b'x'; 64 * 1024];
    fs::write(workspace.path().join("large.bin"), &content).expect("large file");
    let output = std::process::Command::new("sh")
        .args([
            "-c",
            READ_BOUNDED_SCRIPT,
            "--",
            workspace.path().to_str().expect("UTF-8 workspace"),
            "large.bin",
            "65",
        ])
        .output()
        .expect("run bounded read script");
    assert!(output.status.success());
    let separator = output
        .stdout
        .iter()
        .position(|byte| *byte == 0)
        .expect("metadata separator");
    assert_eq!(
        std::str::from_utf8(&output.stdout[..separator])
            .expect("UTF-8 size")
            .trim(),
        "65536"
    );
    assert_eq!(output.stdout.len() - separator - 1, 65);
    let parsed = parse_bounded_read(&output.stdout, 64).expect("parse bounded read");
    assert_eq!(parsed.bytes, vec![b'x'; 64]);
    assert_eq!(parsed.total_bytes, 65_536);
    assert!(parsed.truncated);
}

#[tokio::test]
async fn read_only_command_uses_ssh_while_mutating_command_is_rejected() {
    let fake = FakeSsh::new("cat > \"$0.stdin\"\nprintf inspected");
    let executor = fake.executor();
    let output = executor
        .run_read_only_command(&target(true), "git status --short", Duration::from_secs(5))
        .await
        .expect("trusted read-only command runs");
    assert_eq!(output.stdout, b"inspected");
    assert_eq!(
        fs::read_to_string(&fake.stdin_log).expect("stdin log"),
        "git status --short"
    );
    assert!(matches!(
        executor
            .run_command(&target(true), "git clean -fd", Duration::from_secs(5))
            .await,
        Err(WorkspaceExecutorError::ReadOnly(_))
    ));
}

#[tokio::test]
async fn read_only_rejects_mutations_before_spawning_ssh() {
    let fake = FakeSsh::new("exit 99");
    let executor = fake.executor();
    assert!(matches!(
        executor.write_file(&target(true), "file", b"x").await,
        Err(WorkspaceExecutorError::ReadOnly(id)) if id == "remote-dev"
    ));
    assert!(matches!(
        executor
            .run_command(&target(true), "true", Duration::from_secs(1))
            .await,
        Err(WorkspaceExecutorError::ReadOnly(id)) if id == "remote-dev"
    ));
    assert!(!fake.argv_log.exists());
}

#[tokio::test]
async fn command_timeout_terminates_the_ssh_process() {
    let fake = FakeSsh::new("exec sleep 5");
    let timeout = Duration::from_millis(30);
    let result = fake
        .executor()
        .run_command(&target(false), "true", timeout)
        .await;
    assert!(matches!(result, Err(WorkspaceExecutorError::Timeout(value)) if value == timeout));
}

#[tokio::test]
async fn file_operation_timeout_terminates_the_ssh_process() {
    let fake = FakeSsh::new("exec sleep 5");
    let timeout = Duration::from_millis(30);
    let result = fake
        .executor()
        .with_file_operation_timeout(timeout)
        .read_file(&target(false), "slow.txt")
        .await;
    assert!(matches!(result, Err(WorkspaceExecutorError::Timeout(value)) if value == timeout));
}

#[tokio::test]
async fn structured_query_timeout_terminates_the_ssh_process() {
    let fake = FakeSsh::new("exec sleep 5");
    let timeout = Duration::from_millis(30);
    let result = fake
        .executor()
        .list(
            &target(true),
            WorkspaceListRequest {
                relative_path: ".".into(),
                recursive: false,
                limits: WorkspaceQueryLimits {
                    timeout,
                    ..WorkspaceQueryLimits::default()
                },
            },
        )
        .await;
    assert!(matches!(result, Err(WorkspaceExecutorError::Timeout(value)) if value == timeout));
}

#[test]
fn endpoint_and_relative_path_validation_reject_option_and_traversal_inputs() {
    assert!(
        SshExecutor::new(
            "-oProxyCommand=bad",
            22,
            "agent",
            "/key",
            "/known-hosts",
            "/control"
        )
        .is_err()
    );
    assert!(SshExecutor::new("host", 22, "user;bad", "/key", "/known-hosts", "/control").is_err());
    assert!(validate_relative("../secret").is_err());
    assert!(validate_relative("/absolute").is_err());
}
