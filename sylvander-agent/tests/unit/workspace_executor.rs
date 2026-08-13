use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use super::*;
use crate::execution_context::AgentExecutionContext;
use crate::tool_context::{Cap, ToolContext};
use crate::tools::{CommandTool, EditTool, GitTool, ListTool, ReadTool, SearchTool, WriteTool};

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
    let context = ToolContext::new(AgentExecutionContext::restricted_for("u", "a", "s"))
        .with_executor(
            executor.clone(),
            WorkspaceTarget {
                id: "container:dev".into(),
                workspace_path: "/workspace".into(),
                read_only: false,
            },
        )
        .with_capability(Cap::Read);
    let output = ReadTool::new()
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
async fn conditional_write_fails_closed_without_runtime_coordination() {
    let executor = RecordingExecutor::default();
    let target = WorkspaceTarget {
        id: "uncoordinated".into(),
        workspace_path: "/workspace".into(),
        read_only: false,
    };
    let update = executor
        .read_file_for_update(&target, "file.txt", 1024)
        .await
        .expect("issue revision");
    let error = executor
        .write_file_if_revision(&target, "file.txt", &update.revision, b"replacement", 1024)
        .await
        .expect_err("uncoordinated executor must fail closed");
    assert!(matches!(
        error,
        WorkspaceExecutorError::ConditionalWriteUnavailable(target_id)
            if target_id == "uncoordinated"
    ));
}

#[tokio::test]
async fn every_workspace_tool_fails_closed_when_the_context_has_no_workspace() {
    let context = ToolContext::new(AgentExecutionContext::restricted_for("u", "a", "s"))
        .with_capability(Cap::Read)
        .with_capability(Cap::Write)
        .with_capability(Cap::Spawn)
        .with_capability(Cap::Git);

    let outputs = [
        ReadTool::new()
            .execute(&context, json!({"file_path":"Cargo.toml"}))
            .await
            .unwrap(),
        WriteTool::new()
            .execute(
                &context,
                json!({"file_path":"blocked.txt","content":"blocked"}),
            )
            .await
            .unwrap(),
        EditTool::new()
            .execute(
                &context,
                json!({"file_path":"blocked.txt","old_string":"a","new_string":"b"}),
            )
            .await
            .unwrap(),
        ListTool::new()
            .execute(&context, json!({"path":"."}))
            .await
            .unwrap(),
        SearchTool::new()
            .execute(&context, json!({"query":"needle"}))
            .await
            .unwrap(),
        CommandTool::new()
            .execute(&context, json!({"command":"printf must-not-run"}))
            .await
            .unwrap(),
        GitTool::new()
            .execute(&context, json!({"operation":"status"}))
            .await
            .unwrap(),
    ];

    for output in outputs {
        assert!(output.is_error);
        assert!(
            output.content.contains("workspace path is required"),
            "{}",
            output.content
        );
    }
}

#[tokio::test]
async fn workspace_router_resolves_logical_mounts_and_enforces_capabilities() {
    let task = Arc::new(RecordingExecutor::default());
    let dependency = Arc::new(RecordingExecutor::default());
    let router = WorkspaceRouter::new(
        "task",
        [
            (
                "task".into(),
                MountedWorkspace {
                    executor: task.clone(),
                    target: WorkspaceTarget {
                        id: "local:task".into(),
                        workspace_path: "/task".into(),
                        read_only: false,
                    },
                    capabilities: WorkspaceCapabilities::default(),
                },
            ),
            (
                "dependency".into(),
                MountedWorkspace {
                    executor: dependency.clone(),
                    target: WorkspaceTarget {
                        id: "ssh:dependency".into(),
                        workspace_path: "/dependency".into(),
                        read_only: true,
                    },
                    capabilities: WorkspaceCapabilities {
                        read: false,
                        ..Default::default()
                    },
                },
            ),
        ],
    )
    .unwrap();
    let target = WorkspaceTarget::local("/logical", false);

    assert_eq!(
        router.read_file(&target, "src/lib.rs").await.unwrap(),
        b"from-mock"
    );
    assert_eq!(
        *task.reads.lock().unwrap(),
        [("local:task".into(), "src/lib.rs".into())]
    );
    assert!(
        router
            .read_file(&target, "@dependency/Cargo.toml")
            .await
            .is_err()
    );
    assert!(dependency.reads.lock().unwrap().is_empty());
    assert!(
        router
            .select_mount_target(&target, Some("missing"))
            .is_err()
    );
}

#[tokio::test]
async fn unavailable_target_never_falls_back_to_local() {
    let workspace = tempfile::tempdir().unwrap();
    tokio::fs::write(workspace.path().join("value.txt"), "secret")
        .await
        .unwrap();
    let context = ToolContext::new(AgentExecutionContext::restricted_for("u", "a", "s"))
        .with_execution_target("ssh:build", workspace.path(), false)
        .with_capability(Cap::Read);
    let output = ReadTool::new()
        .execute(&context, json!({"file_path":"value.txt"}))
        .await
        .unwrap();
    assert!(output.is_error);
    assert!(output.content.contains("ssh:build"));
    assert!(output.content.contains("unavailable"));
}
