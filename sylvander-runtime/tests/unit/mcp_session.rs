use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use sylvander_agent::tool::ToolExecutor as _;
use sylvander_agent::tool_context::defaults::system_tool_context;

use super::{
    AgentId, McpServerConfig, SessionId, SessionMcpBinding, SessionMcpError,
    SessionMcpRuntimeService,
};
use crate::execution::{
    ExecutionTargetRegistration, PersistentProcess, PersistentProcessAuthority,
    PersistentProcessEnvironment, PersistentProcessError, PersistentProcessIsolation,
    PersistentProcessSpec, RuntimeExecutionService,
};

#[derive(Clone, Default)]
struct RecordingEnvironment {
    spawns: Arc<Mutex<Vec<(String, PathBuf)>>>,
}

struct ProtocolProcess {
    session_id: String,
    responses: VecDeque<Vec<u8>>,
}

#[async_trait]
impl PersistentProcessEnvironment for RecordingEnvironment {
    fn name(&self) -> &str {
        "sandbox"
    }

    fn isolation(&self) -> PersistentProcessIsolation {
        PersistentProcessIsolation {
            filesystem: true,
            network_denied: true,
            resource_limits: true,
            process_tree: true,
        }
    }

    async fn spawn(
        &self,
        _spec: &PersistentProcessSpec,
        authority: &PersistentProcessAuthority,
    ) -> Result<Box<dyn PersistentProcess>, PersistentProcessError> {
        self.spawns.lock().expect("spawn lock").push((
            authority.owner.session_id.clone(),
            authority.workspace_root.clone(),
        ));
        Ok(Box::new(ProtocolProcess {
            session_id: authority.owner.session_id.clone(),
            responses: VecDeque::new(),
        }))
    }
}

#[async_trait]
impl PersistentProcess for ProtocolProcess {
    async fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), PersistentProcessError> {
        let request: JsonValue = serde_json::from_slice(bytes)
            .map_err(|_| PersistentProcessError::InvalidSpecification("test JSON-RPC"))?;
        let Some(id) = request.get("id").and_then(JsonValue::as_u64) else {
            return Ok(());
        };
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let result = match method {
            "initialize" => json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "isolated", "version": "1"}
            }),
            "tools/list" => json!({"tools": [{
                "name": "identity",
                "description": "Return the owning Session",
                "inputSchema": {"type": "object", "properties": {}}
            }]}),
            "tools/call" => json!({
                "content": [{"type": "text", "text": self.session_id}],
                "isError": false
            }),
            "ping" => json!({}),
            _ => json!({}),
        };
        self.responses.push_back(
            serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
                .expect("response JSON"),
        );
        Ok(())
    }

    async fn read_stdout_frame(&mut self) -> Result<Vec<u8>, PersistentProcessError> {
        self.responses
            .pop_front()
            .ok_or(PersistentProcessError::Closed)
    }

    async fn close_stdin(&mut self) -> Result<(), PersistentProcessError> {
        Ok(())
    }

    async fn wait(&mut self, _timeout: Duration) -> Result<(), PersistentProcessError> {
        Ok(())
    }

    async fn terminate_tree(&mut self) -> Result<(), PersistentProcessError> {
        Ok(())
    }
}

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
    assert!(
        service
            .tool_registry(&SessionId::new("session-a"))
            .is_some_and(|tools| tools.is_empty())
    );
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
    assert!(service.tool_registry(&session_id).is_none());
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

#[tokio::test]
async fn same_agent_sessions_keep_process_workspace_and_results_isolated() {
    let environment = Arc::new(RecordingEnvironment::default());
    let execution = RuntimeExecutionService::new([RuntimeExecutionService::persistent_for_test(
        "sandbox",
        environment.clone(),
    )])
    .expect("execution registry");
    let service = SessionMcpRuntimeService::new(execution, None, None);
    service
        .attach(
            binding("user-a", "session-a"),
            vec![server("identity")],
            "/workspace/a".into(),
        )
        .await
        .expect("attach first Session");
    service
        .attach(
            binding("user-b", "session-b"),
            vec![server("identity")],
            "/workspace/b".into(),
        )
        .await
        .expect("attach second Session");

    let first = service
        .tool_registry(&SessionId::new("session-a"))
        .expect("first catalog");
    let second = service
        .tool_registry(&SessionId::new("session-b"))
        .expect("second catalog");
    let first_call = first
        .prepare("mcp__identity__identity", json!({}))
        .expect("first prepared call");
    let second_call = second
        .prepare("mcp__identity__identity", json!({}))
        .expect("second prepared call");
    let context = system_tool_context();
    let first_output = first
        .get("mcp__identity__identity")
        .expect("first tool")
        .handle(&context, &first_call)
        .await
        .expect("first result");
    let second_output = second
        .get("mcp__identity__identity")
        .expect("second tool")
        .handle(&context, &second_call)
        .await
        .expect("second result");

    assert_eq!(first_output.content, "session-a");
    assert_eq!(second_output.content, "session-b");
    assert_eq!(
        *environment.spawns.lock().expect("spawn records"),
        [
            ("session-a".into(), PathBuf::from("/workspace/a")),
            ("session-b".into(), PathBuf::from("/workspace/b")),
        ]
    );
}
