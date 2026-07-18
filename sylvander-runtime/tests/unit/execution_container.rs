use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use super::*;

struct FakeRuntime {
    directory: TempDir,
    executable: PathBuf,
    arguments: PathBuf,
    started: PathBuf,
    cleanup: PathBuf,
}

impl FakeRuntime {
    fn new() -> Self {
        let directory = TempDir::new().unwrap();
        let executable = directory.path().join("runtime");
        let arguments = directory.path().join("arguments");
        let started = directory.path().join("started");
        let cleanup = directory.path().join("cleanup");
        fs::write(
            &executable,
            format!(
                r#"#!/bin/sh
printf '%s\0' "$@" > '{}'
[ "$1" = rm ] && {{ printf '%s' "$3" > '{}'; exit 0; }}
[ "$1" = run ] || exit 90
shift
mount=
while [ "$#" -gt 0 ]; do
  case $1 in
--rm|--network=none|--interactive|--read-only) shift ;;
--name|--memory|--cpus|--pids-limit|--tmpfs|--security-opt|--cap-drop) shift 2 ;;
--mount) mount=$2; shift 2 ;;
--workdir) shift 2 ;;
*) image=$1; shift; break ;;
  esac
done
workspace=$(printf '%s' "$mount" | sed -n 's/.*source=\([^,]*\),target=.*/\1/p')
[ -n "$workspace" ] || exit 91
cd "$workspace" || exit 92
touch '{}'
exec "$@"
"#,
                arguments.display(),
                cleanup.display(),
                started.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            directory,
            executable,
            arguments,
            started,
            cleanup,
        }
    }

    fn executor(&self) -> ContainerExecutor {
        ContainerExecutor::new(&self.executable, "test/image:latest").unwrap()
    }

    fn target(&self, read_only: bool) -> WorkspaceTarget {
        WorkspaceTarget {
            id: "container".into(),
            workspace_path: self.directory.path().join("workspace"),
            read_only,
        }
    }

    fn argv(&self) -> Vec<String> {
        fs::read(&self.arguments)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
            .map(|field| String::from_utf8_lossy(field).into_owned())
            .collect()
    }
}

#[tokio::test]
async fn file_query_and_command_contract_runs_in_disposable_container() {
    let fake = FakeRuntime::new();
    let workspace = fake.target(false).workspace_path;
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/lib.rs"), "alpha\nneedle\n").unwrap();
    let executor = fake.executor();
    let target = fake.target(false);

    assert_eq!(
        executor.read_file(&target, "src/lib.rs").await.unwrap(),
        b"alpha\nneedle\n"
    );
    let bounded = executor
        .read_file_bounded(&target, "src/lib.rs", 5)
        .await
        .unwrap();
    assert_eq!(bounded.bytes, b"alpha");
    assert!(bounded.truncated);
    assert_eq!(bounded.total_bytes, 13);

    executor
        .write_file(&target, "generated/value.txt", b"written")
        .await
        .unwrap();
    assert_eq!(
        fs::read(workspace.join("generated/value.txt")).unwrap(),
        b"written"
    );

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
    assert!(
        listed
            .entries
            .iter()
            .any(|entry| entry.relative_path == "src/lib.rs"),
        "{listed:?}"
    );
    assert_eq!(
        listed
            .entries
            .iter()
            .find(|entry| entry.relative_path == "src/lib.rs")
            .unwrap()
            .size,
        13
    );
    let searched = executor
        .search(
            &target,
            WorkspaceSearchRequest {
                relative_path: ".".into(),
                query: "needle".into(),
                limits: WorkspaceQueryLimits::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(searched.matches[0].line, "needle");

    let progress = Arc::new(Mutex::new(String::new()));
    let captured = Arc::clone(&progress);
    let output = executor
        .run_command_streaming(
            &target,
            "printf '你好'; printf error >&2",
            Duration::from_secs(2),
            WorkspaceCommandProgressSink::new(move |_, delta| {
                captured.lock().unwrap().push_str(&delta);
            }),
        )
        .await
        .unwrap();
    assert_eq!(output.stdout, "你好".as_bytes());
    assert_eq!(output.stderr, b"error");
    let progress = progress.lock().unwrap().clone();
    assert!(progress.contains("你好"), "{progress}");
    assert!(progress.contains("error"), "{progress}");

    let argv = fake.argv();
    assert!(argv.contains(&"--rm".into()));
    assert!(argv.contains(&"--name".into()));
    assert!(argv.contains(&"--network=none".into()));
    assert!(argv.contains(&"--memory".into()));
    assert!(argv.contains(&"2048m".into()));
    assert!(argv.contains(&"--cpus".into()));
    assert!(argv.contains(&"2.000".into()));
    assert!(argv.contains(&"--pids-limit".into()));
    assert!(argv.contains(&"512".into()));
    assert!(argv.contains(&"--read-only".into()));
    assert!(argv.contains(&"no-new-privileges".into()));
    assert!(argv.contains(&"ALL".into()));
    assert!(argv.contains(&"--interactive".into()));
    assert!(argv.contains(&"test/image:latest".into()));
}

#[tokio::test]
async fn read_only_target_rejects_mutation_and_mounts_read_only_for_inspection() {
    let fake = FakeRuntime::new();
    fs::create_dir_all(fake.target(true).workspace_path).unwrap();
    let executor = fake.executor();
    let target = fake.target(true);

    assert!(matches!(
        executor.write_file(&target, "file", b"x").await,
        Err(WorkspaceExecutorError::ReadOnly(id)) if id == "container"
    ));
    let output = executor
        .run_read_only_command(&target, "printf inspected", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(output.stdout, b"inspected");
    assert!(
        fake.argv()
            .iter()
            .any(|argument| argument.ends_with(",readonly"))
    );
}

#[test]
fn runtime_and_image_cannot_be_interpreted_as_options() {
    assert!(ContainerExecutor::new("-runtime", "image").is_err());
    assert!(ContainerExecutor::new("runtime", "-image").is_err());
    assert!(ContainerExecutor::new("runtime", "image with space").is_err());
}

#[tokio::test]
async fn dropping_an_operation_force_removes_its_named_container() {
    let fake = FakeRuntime::new();
    fs::create_dir_all(fake.target(false).workspace_path).unwrap();
    let executor = fake.executor();
    let target = fake.target(false);
    let operation = tokio::spawn(async move {
        executor
            .run_command(&target, "sleep 30", Duration::from_mins(1))
            .await
    });

    tokio::time::timeout(Duration::from_secs(10), async {
        while !fake.started.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fake container never started");
    operation.abort();
    let _ = operation.await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while !fake.cleanup.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fake container cleanup did not finish");
    let removed = fs::read_to_string(&fake.cleanup).unwrap();
    assert!(removed.starts_with("sylvander-"), "{removed}");
}
