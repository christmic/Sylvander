use std::collections::HashMap;

use super::*;

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
        command: "server".into(),
        args: Vec::new(),
        envs: HashMap::new(),
    }
}

#[test]
fn sessions_with_the_same_server_declaration_keep_distinct_ownership() {
    let service = SessionMcpRuntimeService::new();
    service
        .attach(binding("user-a", "session-a"), vec![server("files")])
        .expect("attach first Session");
    service
        .attach(binding("user-b", "session-b"), vec![server("files")])
        .expect("attach second Session");

    let first = service
        .inspect(&SessionId::new("session-a"))
        .expect("first Session");
    let second = service
        .inspect(&SessionId::new("session-b"))
        .expect("second Session");
    assert_eq!(first.binding.user_id, "user-a");
    assert_eq!(second.binding.user_id, "user-b");
    assert_eq!(first.server_count, 1);
    assert_eq!(second.server_count, 1);
    assert!(first.configured && second.configured);
}

#[test]
fn detach_removes_the_session_before_drain() {
    let service = SessionMcpRuntimeService::new();
    let session_id = SessionId::new("session");
    service
        .attach(binding("user", "session"), vec![server("files")])
        .expect("attach Session");

    service.detach(&session_id);

    assert!(service.inspect(&session_id).is_none());
}

#[test]
fn duplicate_session_or_server_fails_closed() {
    let service = SessionMcpRuntimeService::new();
    service
        .attach(binding("user", "session"), vec![server("files")])
        .expect("attach Session");
    assert_eq!(
        service
            .attach(binding("user", "session"), vec![server("other")])
            .expect_err("duplicate Session"),
        SessionMcpError::DuplicateSession("session".into())
    );

    let duplicate = SessionMcpRuntimeService::new()
        .attach(
            binding("user", "other-session"),
            vec![server("files"), server("files")],
        )
        .expect_err("duplicate server name");
    assert_eq!(duplicate, SessionMcpError::DuplicateServer("files".into()));
}
