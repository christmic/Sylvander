use std::collections::HashMap;

use super::{
    AgentId, McpServerConfig, SessionId, SessionMcpBinding, SessionMcpError,
    SessionMcpRuntimeService,
};
use crate::execution::{ExecutionTargetRegistration, RuntimeExecutionService};

fn service() -> SessionMcpRuntimeService {
    let execution = RuntimeExecutionService::new([ExecutionTargetRegistration::local("sandbox")])
        .expect("fixed execution target");
    SessionMcpRuntimeService::new(execution, None, None)
}

fn binding(user: &str, session: &str) -> SessionMcpBinding {
    SessionMcpBinding {
        user_id: user.into(),
        agent_id: AgentId::new("agent"),
        session_id: SessionId::new(session),
        policy_revision: 7,
    }
}

fn server(name: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.into(),
        execution_environment: "sandbox".into(),
        workspace_access: sylvander_api::McpWorkspaceAccess::Read,
        command: "server".into(),
        args: Vec::new(),
        envs: HashMap::new(),
    }
}

#[tokio::test]
async fn sessions_keep_distinct_ownership_before_servers_are_configured() {
    let service = service();
    service
        .attach(binding("user-a", "session-a"), Vec::new(), "/tmp".into())
        .await
        .expect("attach first Session");
    service
        .attach(binding("user-b", "session-b"), Vec::new(), "/tmp".into())
        .await
        .expect("attach second Session");

    let first = service
        .inspect(&SessionId::new("session-a"))
        .expect("first Session");
    let second = service
        .inspect(&SessionId::new("session-b"))
        .expect("second Session");
    assert_eq!(first.binding.user_id, "user-a");
    assert_eq!(second.binding.user_id, "user-b");
    assert_eq!(first.server_count, 0);
    assert_eq!(second.server_count, 0);
    assert!(first.configured && second.configured);
}

#[tokio::test]
async fn detach_removes_the_session_before_drain() {
    let service = service();
    let session_id = SessionId::new("session");
    service
        .attach(binding("user", "session"), Vec::new(), "/tmp".into())
        .await
        .expect("attach Session");

    service.detach(&session_id).await;

    assert!(service.inspect(&session_id).is_none());
}

#[tokio::test]
async fn duplicate_session_or_server_fails_closed() {
    let runtime = service();
    runtime
        .attach(binding("user", "session"), Vec::new(), "/tmp".into())
        .await
        .expect("attach Session");
    assert_eq!(
        runtime
            .attach(binding("user", "session"), Vec::new(), "/tmp".into())
            .await
            .expect_err("duplicate Session"),
        SessionMcpError::DuplicateSession("session".into())
    );

    let duplicate = service()
        .attach(
            binding("user", "other-session"),
            vec![server("files"), server("files")],
            "/tmp".into(),
        )
        .await
        .expect_err("duplicate server name");
    assert_eq!(duplicate, SessionMcpError::DuplicateServer("files".into()));
}

#[tokio::test]
async fn unknown_execution_environment_fails_before_process_start() {
    let mut declaration = server("files");
    declaration.execution_environment = "missing".into();

    let error = service()
        .attach(binding("user", "session"), vec![declaration], "/tmp".into())
        .await
        .expect_err("unknown environment");
    assert_eq!(
        error,
        SessionMcpError::UnknownEnvironment {
            server: "files".into(),
            environment: "missing".into(),
        }
    );
}
