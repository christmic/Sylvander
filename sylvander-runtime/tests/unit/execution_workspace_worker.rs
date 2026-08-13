use super::*;

#[tokio::test]
async fn proxy_round_trips_results_progress_and_disconnects_fail_closed() {
    let hub = WorkspaceWorkerHub::default();
    let (requests_tx, mut requests_rx) = mpsc::channel(4);
    let hello = WorkspaceWorkerHello {
        protocol_version: WORKSPACE_WORKER_PROTOCOL_VERSION,
        target_id: "macbook".into(),
        workspace_root: "/Users/example/projects".into(),
        allow_local_fallback: false,
    };
    let generation = hub.connect(&hello, requests_tx).await.unwrap();
    let executor = WorkspaceWorkerExecutor {
        target_id: "macbook".into(),
        hub: hub.clone(),
    };
    let target = WorkspaceTarget::local("project", false);
    let observed = Arc::new(std::sync::Mutex::new(String::new()));
    let captured = observed.clone();

    let call = tokio::spawn(async move {
        executor
            .run_command_streaming(
                &target,
                "git status",
                Duration::from_secs(2),
                WorkspaceCommandProgressSink::new(move |_, delta| {
                    captured.lock().unwrap().push_str(&delta);
                }),
            )
            .await
    });
    let WorkspaceWorkerServerMessage::Request { request } = requests_rx.recv().await.unwrap()
    else {
        panic!("request expected")
    };
    hub.event(
        &generation,
        WorkspaceWorkerEvent::Progress {
            request_id: request.request_id.clone(),
            stream: WorkspaceWorkerStream::Stdout,
            delta: "working".into(),
        },
    )
    .await;
    hub.event(
        &generation,
        WorkspaceWorkerEvent::Complete {
            request_id: request.request_id,
            result: WorkspaceWorkerResult::Command {
                success: true,
                status_code: Some(0),
                stdout: b"clean".to_vec(),
                stderr: vec![],
                stdout_truncated: false,
                stderr_truncated: false,
                stdout_total_bytes: 5,
                stderr_total_bytes: 0,
            },
        },
    )
    .await;
    let output = call.await.unwrap().unwrap();
    assert_eq!(output.stdout, b"clean");
    assert_eq!(&*observed.lock().unwrap(), "working");

    let disconnected = WorkspaceWorkerExecutor {
        target_id: "macbook".into(),
        hub: hub.clone(),
    };
    hub.disconnect("macbook", &generation).await;
    let error = disconnected
        .read_file(&WorkspaceTarget::local("project", false), "README.md")
        .await
        .unwrap_err();
    assert!(matches!(error, WorkspaceExecutorError::Unavailable(id) if id == "macbook"));
}

#[tokio::test]
async fn proxy_rejects_absolute_and_parent_workspace_selectors() {
    let executor = WorkspaceWorkerExecutor {
        target_id: "macbook".into(),
        hub: WorkspaceWorkerHub::default(),
    };
    for workspace in ["/Users/example/project", "../project"] {
        let error = executor
            .read_file(&WorkspaceTarget::local(workspace, false), "README.md")
            .await
            .unwrap_err();
        assert!(matches!(error, WorkspaceExecutorError::InvalidPath(_)));
    }
}

#[tokio::test]
async fn dropping_proxy_request_sends_cancel_and_clears_pending_state() {
    let hub = WorkspaceWorkerHub::default();
    let (requests_tx, mut requests_rx) = mpsc::channel(4);
    let generation = hub
        .connect(
            &WorkspaceWorkerHello {
                protocol_version: WORKSPACE_WORKER_PROTOCOL_VERSION,
                target_id: "macbook-cancel".into(),
                workspace_root: "/Users/example/projects".into(),
                allow_local_fallback: false,
            },
            requests_tx,
        )
        .await
        .unwrap();
    let executor = WorkspaceWorkerExecutor {
        target_id: "macbook-cancel".into(),
        hub: hub.clone(),
    };
    let task = tokio::spawn(async move {
        executor
            .read_file(&WorkspaceTarget::local("project", false), "README.md")
            .await
    });
    let WorkspaceWorkerServerMessage::Request { request } = requests_rx.recv().await.unwrap()
    else {
        panic!("request expected")
    };
    task.abort();
    let cancelled = requests_rx.recv().await.unwrap();
    assert_eq!(
        cancelled,
        WorkspaceWorkerServerMessage::Cancel {
            request_id: request.request_id
        }
    );
    assert!(hub.state.lock().await.pending.is_empty());
    hub.disconnect("macbook-cancel", &generation).await;
}

#[tokio::test]
async fn worker_registration_rejects_a_fallback_that_would_overstate_isolation() {
    let hub = WorkspaceWorkerHub::default();
    let (requests, _) = mpsc::channel(1);
    assert!(
        hub.connect(
            &WorkspaceWorkerHello {
                protocol_version: WORKSPACE_WORKER_PROTOCOL_VERSION,
                target_id: "unsafe-fallback".into(),
                workspace_root: "/Users/example/projects".into(),
                allow_local_fallback: true,
            },
            requests,
        )
        .await
        .is_err()
    );
}
