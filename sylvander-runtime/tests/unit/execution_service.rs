#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use sylvander_agent::workspace_executor::WorkspaceExecutor;

use super::{
    ContainerExecutor, ExecutionHealthTask, ExecutionServiceError, ExecutionTargetKind,
    ExecutionTargetRegistration, ExecutionTargetStatus, LocalExecutor,
    PersistentProcessEnvironment, RuntimeExecutionService, UnavailablePersistentProcessEnvironment,
};

fn unavailable(name: &str) -> Arc<dyn PersistentProcessEnvironment> {
    Arc::new(UnavailablePersistentProcessEnvironment::new(name))
}

#[test]
fn target_registry_rejects_blank_and_duplicate_identifiers() {
    let local = Arc::new(LocalExecutor) as Arc<dyn WorkspaceExecutor>;
    assert!(matches!(
        RuntimeExecutionService::new([ExecutionTargetRegistration {
            target_id: " ".into(),
            kind: ExecutionTargetKind::Local,
            status: ExecutionTargetStatus::Unverified,
            executor: local.clone(),
            persistent_processes: unavailable("blank"),
            probe: None,
            local_fallback: false,
            worker_channel_instance: None,
        }]),
        Err(ExecutionServiceError::InvalidTargetId)
    ));
    assert!(matches!(
        RuntimeExecutionService::new([
            ExecutionTargetRegistration {
                target_id: "local".into(),
                kind: ExecutionTargetKind::Local,
                status: ExecutionTargetStatus::Unverified,
                executor: local.clone(),
                persistent_processes: unavailable("local"),
                probe: None,
                local_fallback: false,
                worker_channel_instance: None,
            },
            ExecutionTargetRegistration {
                target_id: "local".into(),
                kind: ExecutionTargetKind::Local,
                status: ExecutionTargetStatus::Unverified,
                executor: local,
                persistent_processes: unavailable("local"),
                probe: None,
                local_fallback: false,
                worker_channel_instance: None,
            },
        ]),
        Err(ExecutionServiceError::DuplicateTargetId)
    ));
}

#[test]
fn health_is_sorted_and_never_calls_unconfined_targets_sandboxes() {
    let service = RuntimeExecutionService::new([
        ExecutionTargetRegistration {
            target_id: "ssh:build".into(),
            kind: ExecutionTargetKind::Ssh,
            status: ExecutionTargetStatus::Unverified,
            executor: Arc::new(LocalExecutor),
            persistent_processes: unavailable("ssh:build"),
            probe: None,
            local_fallback: false,
            worker_channel_instance: None,
        },
        ExecutionTargetRegistration {
            target_id: "container:review".into(),
            kind: ExecutionTargetKind::Container,
            status: ExecutionTargetStatus::Unverified,
            executor: Arc::new(ContainerExecutor::new("docker", "review:latest").unwrap()),
            persistent_processes: unavailable("container:review"),
            probe: None,
            local_fallback: false,
            worker_channel_instance: None,
        },
        ExecutionTargetRegistration {
            target_id: "local".into(),
            kind: ExecutionTargetKind::Local,
            status: ExecutionTargetStatus::Ready,
            executor: Arc::new(LocalExecutor),
            persistent_processes: unavailable("local"),
            probe: None,
            local_fallback: false,
            worker_channel_instance: None,
        },
    ])
    .unwrap();

    let health = service.health();
    assert_eq!(
        health
            .iter()
            .map(|target| target.target_id.as_str())
            .collect::<Vec<_>>(),
        ["container:review", "local", "ssh:build"]
    );
    assert!(health[0].sandbox_enforced);
    assert!(health[0].process_tree);
    assert!(!health[1].sandbox_enforced);
    assert!(!health[1].process_tree);
    assert!(!health[2].sandbox_enforced);
    assert!(!health[2].process_tree);
    assert_eq!(health[0].status, ExecutionTargetStatus::Unverified);
    assert_eq!(health[1].status, ExecutionTargetStatus::Ready);
}

#[cfg(unix)]
#[tokio::test]
async fn probes_promote_success_and_retain_content_free_failure_counts() {
    let directory = tempfile::TempDir::new().unwrap();
    let executable = directory.path().join("container-runtime");
    std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).unwrap();
    let available = Arc::new(ContainerExecutor::new(&executable, "review:latest").unwrap());
    let missing = Arc::new(
        ContainerExecutor::new(directory.path().join("missing-runtime"), "review:latest").unwrap(),
    );
    let service = RuntimeExecutionService::new([
        ExecutionTargetRegistration::container(
            "container:ok",
            available,
            unavailable("container:ok"),
        ),
        ExecutionTargetRegistration::container(
            "container:missing",
            missing,
            unavailable("container:missing"),
        ),
    ])
    .unwrap();

    service.probe_all().await;
    service.probe_all().await;
    let health = service.health();
    let missing = health
        .iter()
        .find(|target| target.target_id == "container:missing")
        .unwrap();
    let ready = health
        .iter()
        .find(|target| target.target_id == "container:ok")
        .unwrap();
    assert_eq!(missing.status, ExecutionTargetStatus::Degraded);
    assert_eq!(missing.probe_failures, 2);
    assert_eq!(missing.last_probe_succeeded, Some(false));
    assert_eq!(ready.status, ExecutionTargetStatus::Ready);
    assert_eq!(ready.probe_failures, 0);
    assert_eq!(ready.last_probe_succeeded, Some(true));
}

#[cfg(unix)]
#[tokio::test]
async fn background_probe_is_owned_and_shutdown_is_joined() {
    let directory = tempfile::TempDir::new().unwrap();
    let missing = Arc::new(
        ContainerExecutor::new(directory.path().join("missing-runtime"), "review:latest").unwrap(),
    );
    let service = RuntimeExecutionService::new([ExecutionTargetRegistration::container(
        "container:missing",
        missing,
        unavailable("container:missing"),
    )])
    .unwrap();
    let task = ExecutionHealthTask::start(service.clone());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if service.health()[0].status == ExecutionTargetStatus::Degraded {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();

    task.shutdown().await;
    assert!(task.stop.lock().await.is_none());
    assert!(task.task.lock().await.is_none());
}
