//! Minimal MCP stdio transport and [`Tool`](crate::tool::Tool) adapter.
//!
//! The transport owns one server process and serializes JSON-RPC requests over
//! newline-delimited JSON-RPC on stdin/stdout. Composition code can connect once,
//! discover the tools, and register the returned [`McpTool`] values in the
//! ordinary [`ToolRegistry`](crate::tool::ToolRegistry).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::spec::McpServerConfig;
use crate::tool::{DynamicToolSource, Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;
const TOOL_RESULT_HEAD_BYTES: usize = 16 * 1024;
const MCP_HEALTH_ACTIVE: u8 = 1;
const MCP_HEALTH_DEGRADED: u8 = 2;
const MCP_HEALTH_UNAVAILABLE: u8 = 3;
const MCP_HEALTH_INTERVAL: Duration = Duration::from_secs(30);

/// Errors raised while starting or communicating with an MCP server.
#[derive(Debug, Error)]
pub enum McpError {
    #[error("failed to start MCP server {server}: {source}")]
    Spawn {
        server: String,
        #[source]
        source: std::io::Error,
    },
    #[error("MCP server {server} closed its output")]
    Closed { server: String },
    #[error("MCP server {server} I/O failed: {source}")]
    Io {
        server: String,
        #[source]
        source: std::io::Error,
    },
    #[error("MCP server {server} sent an invalid frame: {message}")]
    InvalidFrame { server: String, message: String },
    #[error("MCP server {server} sent invalid JSON: {source}")]
    InvalidJson {
        server: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("MCP server {server} request {method} timed out after {duration:?}")]
    Timeout {
        server: String,
        method: String,
        duration: Duration,
    },
    #[error("MCP server {server} rejected {method}: {message}")]
    Rpc {
        server: String,
        method: String,
        message: String,
    },
    #[error("MCP server {server} returned an invalid {method} result: {message}")]
    InvalidResult {
        server: String,
        method: String,
        message: String,
    },
}

struct ProcessIo {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

struct McpInner {
    server_name: String,
    config: McpServerConfig,
    request_timeout: Duration,
    next_id: AtomicU64,
    generation: AtomicU64,
    reconnect: Mutex<()>,
    io: Mutex<ProcessIo>,
    child: Mutex<Child>,
    result_artifact_root: Option<PathBuf>,
    tool_definitions: std::sync::RwLock<Vec<JsonValue>>,
    resource_definitions: std::sync::RwLock<Vec<JsonValue>>,
    supports_resources: AtomicBool,
    health: AtomicU8,
    reconnect_count: AtomicU64,
    cancellation_count: AtomicU64,
    shutdown: AtomicBool,
}

/// A connected MCP stdio server.
#[derive(Clone)]
pub struct McpStdioClient {
    inner: Arc<McpInner>,
}

struct PendingRequest {
    client: McpStdioClient,
    id: u64,
    armed: bool,
}

impl PendingRequest {
    fn new(client: McpStdioClient, id: u64) -> Self {
        Self {
            client,
            id,
            armed: true,
        }
    }

    fn complete(&mut self) {
        self.armed = false;
    }

    async fn cancel(&mut self, reason: &'static str) {
        if self.armed {
            self.armed = false;
            self.client.send_cancellation(self.id, reason).await;
        }
    }
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let client = self.client.clone();
        let id = self.id;
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                client
                    .send_cancellation(id, "client request interrupted")
                    .await;
            });
        }
    }
}

impl std::fmt::Debug for McpStdioClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpStdioClient")
            .field("server_name", &self.inner.server_name)
            .field("request_timeout", &self.inner.request_timeout)
            .finish_non_exhaustive()
    }
}

impl McpStdioClient {
    /// Start a server, complete the MCP handshake, and return a live client.
    pub async fn connect(
        config: &McpServerConfig,
        request_timeout: Duration,
    ) -> Result<Self, McpError> {
        Self::connect_inner(config, request_timeout, None, true).await
    }

    /// Start a server and persist every complete tool result below `root`.
    ///
    /// Callers still receive a bounded summary. The durable JSON artifact is
    /// retained for later inspection, debugging, and evidence-driven
    /// improvement without flooding the model or UI.
    pub async fn connect_with_result_artifacts(
        config: &McpServerConfig,
        request_timeout: Duration,
        root: PathBuf,
    ) -> Result<Self, McpError> {
        Self::connect_inner(config, request_timeout, Some(root), true).await
    }

    async fn connect_inner(
        config: &McpServerConfig,
        request_timeout: Duration,
        result_artifact_root: Option<PathBuf>,
        start_health_monitor: bool,
    ) -> Result<Self, McpError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .envs(&config.envs)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        let mut child = command.spawn().map_err(|source| McpError::Spawn {
            server: config.name.clone(),
            source,
        })?;
        let stdin = child.stdin.take().ok_or_else(|| McpError::InvalidFrame {
            server: config.name.clone(),
            message: "child stdin was not piped".into(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| McpError::InvalidFrame {
            server: config.name.clone(),
            message: "child stdout was not piped".into(),
        })?;

        let client = Self {
            inner: Arc::new(McpInner {
                server_name: config.name.clone(),
                config: config.clone(),
                request_timeout,
                next_id: AtomicU64::new(1),
                generation: AtomicU64::new(1),
                reconnect: Mutex::new(()),
                io: Mutex::new(ProcessIo {
                    stdin,
                    stdout: BufReader::new(stdout),
                }),
                child: Mutex::new(child),
                result_artifact_root,
                tool_definitions: std::sync::RwLock::new(Vec::new()),
                resource_definitions: std::sync::RwLock::new(Vec::new()),
                supports_resources: AtomicBool::new(false),
                health: AtomicU8::new(MCP_HEALTH_ACTIVE),
                reconnect_count: AtomicU64::new(0),
                cancellation_count: AtomicU64::new(0),
                shutdown: AtomicBool::new(false),
            }),
        };

        let initialized = client
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "sylvander", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        let negotiated = initialized
            .get("protocolVersion")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if negotiated != MCP_PROTOCOL_VERSION {
            return Err(McpError::InvalidResult {
                server: config.name.clone(),
                method: "initialize".into(),
                message: format!(
                    "server selected unsupported protocol {negotiated:?}; expected {MCP_PROTOCOL_VERSION}"
                ),
            });
        }
        client.inner.supports_resources.store(
            initialized
                .get("capabilities")
                .and_then(|capabilities| capabilities.get("resources"))
                .is_some(),
            Ordering::Release,
        );
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        if start_health_monitor {
            spawn_health_monitor(&client);
        }
        Ok(client)
    }

    /// Discover all tools currently advertised by the connected server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| McpError::InvalidResult {
                server: self.inner.server_name.clone(),
                method: "tools/list".into(),
                message: "missing tools array".into(),
            })?;

        let discovered = tools
            .iter()
            .map(|definition| McpTool::from_definition(self.clone(), definition))
            .collect::<Result<Vec<_>, _>>()?;
        self.inner
            .tool_definitions
            .write()
            .unwrap()
            .clone_from(tools);
        self.inner
            .health
            .store(MCP_HEALTH_ACTIVE, Ordering::Release);
        if self.inner.supports_resources.load(Ordering::Acquire) {
            self.refresh_resources().await?;
        }
        Ok(discovered)
    }

    fn current_tools(&self) -> Vec<McpTool> {
        self.inner
            .tool_definitions
            .read()
            .unwrap()
            .iter()
            .filter_map(|definition| McpTool::from_definition(self.clone(), definition).ok())
            .collect()
    }

    fn resource_tools(&self) -> Vec<Arc<dyn Tool>> {
        if !self.inner.supports_resources.load(Ordering::Acquire) {
            return Vec::new();
        }
        [McpResourceOperation::List, McpResourceOperation::Read]
            .into_iter()
            .map(|operation| {
                Arc::new(McpResourceTool::new(self.clone(), operation)) as Arc<dyn Tool>
            })
            .collect()
    }

    async fn refresh_resources(&self) -> Result<(), McpError> {
        const MAX_RESOURCE_PAGES: usize = 32;
        const MAX_RESOURCES: usize = 4096;

        let mut resources = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_RESOURCE_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let result = self.request("resources/list", params).await?;
            let page = result
                .get("resources")
                .and_then(JsonValue::as_array)
                .ok_or_else(|| McpError::InvalidResult {
                    server: self.inner.server_name.clone(),
                    method: "resources/list".into(),
                    message: "missing resources array".into(),
                })?;
            if resources.len().saturating_add(page.len()) > MAX_RESOURCES {
                return Err(McpError::InvalidResult {
                    server: self.inner.server_name.clone(),
                    method: "resources/list".into(),
                    message: format!("resource catalog exceeds {MAX_RESOURCES} entries"),
                });
            }
            resources.extend(page.iter().cloned());
            let next = result
                .get("nextCursor")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            if next.is_none() {
                *self.inner.resource_definitions.write().unwrap() = resources;
                return Ok(());
            }
            if next == cursor {
                return Err(McpError::InvalidResult {
                    server: self.inner.server_name.clone(),
                    method: "resources/list".into(),
                    message: "resource pagination cursor did not advance".into(),
                });
            }
            cursor = next;
        }
        Err(McpError::InvalidResult {
            server: self.inner.server_name.clone(),
            method: "resources/list".into(),
            message: format!("resource catalog exceeds {MAX_RESOURCE_PAGES} pages"),
        })
    }

    async fn read_resource(&self, uri: &str, session_id: &str) -> Result<ToolOutput, McpError> {
        let result = self
            .request("resources/read", json!({ "uri": uri }))
            .await?;
        let artifact = match &self.inner.result_artifact_root {
            Some(root) => persist_result_artifact(
                root,
                session_id,
                &self.inner.server_name,
                "read_resource",
                &result,
            )
            .await
            .ok(),
            None => None,
        };
        Ok(map_tool_result(&result, artifact.as_deref()))
    }

    /// Stop the child process and wait for it to exit.
    pub async fn shutdown(&self) -> Result<(), McpError> {
        self.inner.shutdown.store(true, Ordering::Release);
        let mut child = self.inner.child.lock().await;
        let result = stop_child(&self.inner.server_name, &mut child).await;
        self.inner
            .health
            .store(MCP_HEALTH_UNAVAILABLE, Ordering::Release);
        result
    }

    /// Probe the MCP transport without exposing server content.
    pub async fn probe_health(&self) -> Result<(), McpError> {
        let generation = self.inner.generation.load(Ordering::Acquire);
        match self.request("ping", json!({})).await {
            Ok(_) => {
                self.inner
                    .health
                    .store(MCP_HEALTH_ACTIVE, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                self.inner
                    .health
                    .store(MCP_HEALTH_DEGRADED, Ordering::Release);
                if is_recoverable_transport_error(&error) {
                    self.reconnect_if_current(generation).await?;
                }
                Err(error)
            }
        }
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: JsonValue,
        session_id: &str,
    ) -> Result<ToolOutput, McpError> {
        let generation = self.inner.generation.load(Ordering::Acquire);
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        match result {
            Ok(result) => {
                self.inner
                    .health
                    .store(MCP_HEALTH_ACTIVE, Ordering::Release);
                let artifact = match &self.inner.result_artifact_root {
                    Some(root) => {
                        match persist_result_artifact(
                            root,
                            session_id,
                            &self.inner.server_name,
                            name,
                            &result,
                        )
                        .await
                        {
                            Ok(path) => Some(path),
                            Err(error) => {
                                tracing::warn!(
                                    server = %self.inner.server_name,
                                    tool = name,
                                    session_id,
                                    error = %error,
                                    "failed to persist complete MCP result artifact"
                                );
                                None
                            }
                        }
                    }
                    None => None,
                };
                Ok(map_tool_result(&result, artifact.as_deref()))
            }
            Err(error) => {
                if is_recoverable_transport_error(&error) {
                    if self.reconnect_if_current(generation).await.is_err() {
                        self.inner
                            .health
                            .store(MCP_HEALTH_DEGRADED, Ordering::Release);
                    }
                } else {
                    self.inner
                        .health
                        .store(MCP_HEALTH_DEGRADED, Ordering::Release);
                }
                Err(error)
            }
        }
    }

    async fn reconnect_if_current(&self, observed_generation: u64) -> Result<(), McpError> {
        let _reconnect = self.inner.reconnect.lock().await;
        if self.inner.generation.load(Ordering::Acquire) != observed_generation {
            return Ok(());
        }
        let replacement = Self::connect_inner(
            &self.inner.config,
            self.inner.request_timeout,
            self.inner.result_artifact_root.clone(),
            false,
        )
        .await?;
        let refreshed = replacement.list_tools().await?;
        drop(refreshed);
        let replacement =
            Arc::try_unwrap(replacement.inner).map_err(|_| McpError::InvalidResult {
                server: self.inner.server_name.clone(),
                method: "reconnect".into(),
                message: "replacement process is unexpectedly shared".into(),
            })?;
        let supports_resources = replacement.supports_resources.load(Ordering::Acquire);
        let tool_definitions = replacement.tool_definitions.into_inner().unwrap();
        let resource_definitions = replacement.resource_definitions.into_inner().unwrap();
        let new_io = replacement.io.into_inner();
        let new_child = replacement.child.into_inner();

        let mut io = self.inner.io.lock().await;
        let mut child = self.inner.child.lock().await;
        stop_child(&self.inner.server_name, &mut child).await?;
        *io = new_io;
        *child = new_child;
        *self.inner.tool_definitions.write().unwrap() = tool_definitions;
        *self.inner.resource_definitions.write().unwrap() = resource_definitions;
        self.inner
            .supports_resources
            .store(supports_resources, Ordering::Release);
        self.inner.generation.fetch_add(1, Ordering::Release);
        self.inner.reconnect_count.fetch_add(1, Ordering::Relaxed);
        self.inner
            .health
            .store(MCP_HEALTH_ACTIVE, Ordering::Release);
        Ok(())
    }

    async fn request(&self, method: &str, params: JsonValue) -> Result<JsonValue, McpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let duration = self.inner.request_timeout;
        let mut pending = PendingRequest::new(self.clone(), id);
        if let Ok(result) = timeout(duration, self.request_inner(id, method, &request)).await {
            pending.complete();
            result
        } else {
            pending.cancel("client request timed out").await;
            Err(McpError::Timeout {
                server: self.inner.server_name.clone(),
                method: method.into(),
                duration,
            })
        }
    }

    async fn send_cancellation(&self, request_id: u64, reason: &'static str) {
        self.inner
            .cancellation_count
            .fetch_add(1, Ordering::Relaxed);
        let _ = timeout(
            Duration::from_secs(1),
            self.notify(
                "notifications/cancelled",
                json!({ "requestId": request_id, "reason": reason }),
            ),
        )
        .await;
    }

    async fn request_inner(
        &self,
        id: u64,
        method: &str,
        request: &JsonValue,
    ) -> Result<JsonValue, McpError> {
        let mut io = self.inner.io.lock().await;
        write_frame(&mut io.stdin, request)
            .await
            .map_err(|source| self.io_error(source))?;

        loop {
            let response = read_frame(&mut io.stdout, &self.inner.server_name).await?;
            if response.get("id").and_then(JsonValue::as_u64) != Some(id) {
                // Server notifications may arrive between a request and response.
                continue;
            }
            if let Some(error) = response.get("error") {
                let message = error
                    .get("message")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("unknown JSON-RPC error");
                return Err(McpError::Rpc {
                    server: self.inner.server_name.clone(),
                    method: method.into(),
                    message: message.into(),
                });
            }
            return response
                .get("result")
                .cloned()
                .ok_or_else(|| McpError::InvalidResult {
                    server: self.inner.server_name.clone(),
                    method: method.into(),
                    message: "response has neither result nor error".into(),
                });
        }
    }

    async fn notify(&self, method: &str, params: JsonValue) -> Result<(), McpError> {
        let notification = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let mut io = self.inner.io.lock().await;
        write_frame(&mut io.stdin, &notification)
            .await
            .map_err(|source| self.io_error(source))
    }

    fn io_error(&self, source: std::io::Error) -> McpError {
        McpError::Io {
            server: self.inner.server_name.clone(),
            source,
        }
    }
}

fn spawn_health_monitor(client: &McpStdioClient) {
    let inner = Arc::downgrade(&client.inner);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(MCP_HEALTH_INTERVAL).await;
            let Some(inner) = inner.upgrade() else {
                break;
            };
            if inner.shutdown.load(Ordering::Acquire) {
                break;
            }
            let client = McpStdioClient { inner };
            let _ = client.probe_health().await;
        }
    });
}

impl DynamicToolSource for McpStdioClient {
    fn snapshot(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = self
            .current_tools()
            .into_iter()
            .map(|tool| Arc::new(tool) as Arc<dyn Tool>)
            .collect::<Vec<_>>();
        tools.extend(self.resource_tools());
        tools
    }

    fn platform_feature(&self) -> Option<sylvander_protocol::PlatformFeature> {
        use sylvander_protocol::{
            PlatformAuthStatus, PlatformFeature, PlatformFeatureKind, PlatformFeatureStatus,
            PlatformTrust,
        };

        let status = match self.inner.health.load(Ordering::Acquire) {
            MCP_HEALTH_ACTIVE => PlatformFeatureStatus::Active,
            MCP_HEALTH_DEGRADED => PlatformFeatureStatus::Degraded,
            _ => PlatformFeatureStatus::Unavailable,
        };
        let tool_count = self.inner.tool_definitions.read().unwrap().len();
        let resource_count = self.inner.resource_definitions.read().unwrap().len();
        let generation = self.inner.generation.load(Ordering::Acquire);
        let reconnects = self.inner.reconnect_count.load(Ordering::Acquire);
        let cancellations = self.inner.cancellation_count.load(Ordering::Acquire);
        Some(PlatformFeature {
            kind: PlatformFeatureKind::Mcp,
            name: self.inner.server_name.clone(),
            status,
            summary: format!(
                "{tool_count} tools · {resource_count} resources · generation {generation} · \
                 {reconnects} reconnects · {cancellations} cancellations"
            ),
            source: std::path::Path::new(&self.inner.config.command)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string),
            trust: Some(PlatformTrust::External),
            auth: if self.inner.config.envs.is_empty() {
                PlatformAuthStatus::NotRequired
            } else {
                PlatformAuthStatus::Configured
            },
            capabilities: if self.inner.supports_resources.load(Ordering::Acquire) {
                vec!["tools".into(), "resources".into()]
            } else {
                vec!["tools".into()]
            },
            reloadable: true,
        })
    }
}

fn is_recoverable_transport_error(error: &McpError) -> bool {
    matches!(
        error,
        McpError::Closed { .. } | McpError::Io { .. } | McpError::Timeout { .. }
    )
}

async fn stop_child(server: &str, child: &mut Child) -> Result<(), McpError> {
    match child.try_wait() {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => {}
        Err(source) => {
            return Err(McpError::Io {
                server: server.into(),
                source,
            });
        }
    }
    child.kill().await.map_err(|source| McpError::Io {
        server: server.into(),
        source,
    })?;
    child.wait().await.map_err(|source| McpError::Io {
        server: server.into(),
        source,
    })?;
    Ok(())
}

/// A discovered MCP tool adapted to Sylvander's ordinary tool interface.
#[derive(Debug, Clone)]
pub struct McpTool {
    client: McpStdioClient,
    name: String,
    remote_name: String,
    description: String,
    input_schema: InputSchema,
}

impl McpTool {
    fn from_definition(client: McpStdioClient, definition: &JsonValue) -> Result<Self, McpError> {
        let server = client.inner.server_name.clone();
        let remote_name = definition
            .get("name")
            .and_then(JsonValue::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| McpError::InvalidResult {
                server: server.clone(),
                method: "tools/list".into(),
                message: "tool is missing a name".into(),
            })?
            .to_owned();
        let name = namespaced_tool_name(&server, &remote_name);
        let description = definition
            .get("description")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned();
        let input_schema = definition
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object" }));
        if !input_schema.is_object() {
            return Err(McpError::InvalidResult {
                server,
                method: "tools/list".into(),
                message: format!("tool {name} inputSchema is not an object"),
            });
        }
        Ok(Self {
            client,
            name,
            remote_name,
            description,
            input_schema: InputSchema::from_json_value(input_schema),
        })
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> InputSchema {
        self.input_schema.clone()
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        self.client
            .call_tool(&self.remote_name, input, &ctx.session_id().0)
            .await
            .map_err(|error| match error {
                McpError::Timeout { duration, .. } => ToolError::Timeout(duration),
                other => ToolError::Other(other.to_string()),
            })
    }
}

#[derive(Debug, Clone, Copy)]
enum McpResourceOperation {
    List,
    Read,
}

#[derive(Debug, Clone)]
struct McpResourceTool {
    client: McpStdioClient,
    name: String,
    operation: McpResourceOperation,
}

impl McpResourceTool {
    fn new(client: McpStdioClient, operation: McpResourceOperation) -> Self {
        let remote_name = match operation {
            McpResourceOperation::List => "list_resources",
            McpResourceOperation::Read => "read_resource",
        };
        let name = namespaced_tool_name(&client.inner.server_name, remote_name);
        Self {
            client,
            name,
            operation,
        }
    }
}

#[async_trait]
impl Tool for McpResourceTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        match self.operation {
            McpResourceOperation::List => "List resources currently advertised by this MCP server.",
            McpResourceOperation::Read => {
                "Read one MCP resource by its exact URI. Use list_resources first when needed."
            }
        }
    }

    fn input_schema(&self) -> InputSchema {
        match self.operation {
            McpResourceOperation::List => InputSchema::from_json_value(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            })),
            McpResourceOperation::Read => InputSchema::from_json_value(json!({
                "type": "object",
                "properties": {
                    "uri": {
                        "type": "string",
                        "description": "Exact URI returned by list_resources"
                    }
                },
                "required": ["uri"],
                "additionalProperties": false
            })),
        }
    }

    async fn execute(&self, ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        match self.operation {
            McpResourceOperation::List => {
                self.client
                    .refresh_resources()
                    .await
                    .map_err(|error| ToolError::Other(error.to_string()))?;
                let resources = self.client.inner.resource_definitions.read().unwrap();
                Ok(ToolOutput::ok(bound_tool_result(
                    json!({ "resources": &*resources }).to_string(),
                )))
            }
            McpResourceOperation::Read => {
                let uri = input
                    .get("uri")
                    .and_then(JsonValue::as_str)
                    .filter(|uri| !uri.is_empty())
                    .ok_or_else(|| ToolError::Other("resource URI is required".into()))?;
                self.client
                    .read_resource(uri, &ctx.session_id().0)
                    .await
                    .map_err(|error| ToolError::Other(error.to_string()))
            }
        }
    }
}

fn namespaced_tool_name(server: &str, remote_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        bounded_name_component(server, 20),
        bounded_name_component(remote_name, 34)
    )
}

fn bounded_name_component(value: &str, max_len: usize) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let sanitized = if sanitized.is_empty() {
        "unnamed".to_owned()
    } else {
        sanitized
    };
    if sanitized == value && sanitized.len() <= max_len {
        return sanitized;
    }

    let digest = Sha256::digest(value.as_bytes());
    let suffix = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    );
    let head_len = max_len.saturating_sub(suffix.len() + 1);
    let head = sanitized.chars().take(head_len).collect::<String>();
    format!("{head}_{suffix}")
}

async fn write_frame(writer: &mut ChildStdin, value: &JsonValue) -> std::io::Result<()> {
    let body = serde_json::to_vec(value).expect("serializing JSON values cannot fail");
    writer.write_all(&body).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

async fn read_frame(
    reader: &mut BufReader<ChildStdout>,
    server: &str,
) -> Result<JsonValue, McpError> {
    let mut line = Vec::new();
    let bytes = reader
        .read_until(b'\n', &mut line)
        .await
        .map_err(|source| McpError::Io {
            server: server.into(),
            source,
        })?;
    if bytes == 0 {
        return Err(McpError::Closed {
            server: server.into(),
        });
    }
    if line.len() > MAX_FRAME_BYTES {
        return Err(McpError::InvalidFrame {
            server: server.into(),
            message: format!(
                "message is {} bytes; limit is {MAX_FRAME_BYTES}",
                line.len()
            ),
        });
    }
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
    serde_json::from_slice(&line).map_err(|source| McpError::InvalidJson {
        server: server.into(),
        source,
    })
}

fn map_tool_result(result: &JsonValue, artifact: Option<&Path>) -> ToolOutput {
    let is_error = result
        .get("isError")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let mut parts = result
        .get("content")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .map(|part| {
            if part.get("type").and_then(JsonValue::as_str) == Some("text") {
                part.get("text")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_owned()
            } else {
                let mut summary = part.clone();
                redact_binary_payloads(&mut summary);
                serde_json::to_string(&summary).unwrap_or_else(|_| "<invalid MCP content>".into())
            }
        })
        .collect::<Vec<_>>();
    if parts.is_empty()
        && let Some(structured) = result.get("structuredContent")
    {
        parts.push(
            serde_json::to_string_pretty(structured)
                .unwrap_or_else(|_| "<invalid MCP structured content>".into()),
        );
    }
    if parts.is_empty() {
        parts.push(
            serde_json::to_string_pretty(result).unwrap_or_else(|_| "<invalid MCP result>".into()),
        );
    }
    let content = match artifact {
        Some(path) => {
            let suffix = format!("\n\nFull result artifact: {}", path.display());
            let summary_limit = MAX_TOOL_RESULT_BYTES.saturating_sub(suffix.len());
            format!(
                "{}{suffix}",
                bound_tool_result_to_limit(parts.join("\n"), summary_limit)
            )
        }
        None => bound_tool_result(parts.join("\n")),
    };
    if is_error {
        ToolOutput::err(content)
    } else {
        ToolOutput::ok(content)
    }
}

async fn persist_result_artifact(
    root: &Path,
    session_id: &str,
    server: &str,
    tool: &str,
    result: &JsonValue,
) -> std::io::Result<PathBuf> {
    let directory = root
        .join(safe_path_component(session_id))
        .join(safe_path_component(server));
    tokio::fs::create_dir_all(&directory).await?;
    let id = uuid::Uuid::new_v4();
    let filename = format!("{}-{id}.json", safe_path_component(tool));
    let path = directory.join(filename);
    let temporary = path.with_extension("json.tmp");
    let body =
        serde_json::to_vec_pretty(result).expect("serializing an MCP JSON result cannot fail");
    tokio::fs::write(&temporary, body).await?;
    tokio::fs::rename(&temporary, &path).await?;
    Ok(path)
}

fn safe_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() || sanitized == "." || sanitized == ".." {
        "_".into()
    } else {
        sanitized
    }
}

fn redact_binary_payloads(value: &mut JsonValue) {
    match value {
        JsonValue::Object(object) => {
            for key in ["data", "blob"] {
                if let Some(payload) = object.get_mut(key)
                    && let Some(encoded) = payload.as_str()
                {
                    *payload =
                        JsonValue::String(format!("<omitted {} encoded bytes>", encoded.len()));
                }
            }
            for child in object.values_mut() {
                redact_binary_payloads(child);
            }
        }
        JsonValue::Array(values) => {
            for child in values {
                redact_binary_payloads(child);
            }
        }
        _ => {}
    }
}

fn bound_tool_result(content: String) -> String {
    bound_tool_result_to_limit(content, MAX_TOOL_RESULT_BYTES)
}

fn bound_tool_result_to_limit(content: String, limit: usize) -> String {
    if content.len() <= limit {
        return content;
    }
    let marker = format!(
        "\n… MCP result truncated: {} bytes total …\n",
        content.len()
    );
    let available = limit.saturating_sub(marker.len());
    let head_end = floor_char_boundary(&content, TOOL_RESULT_HEAD_BYTES.min(available));
    let tail_bytes = available.saturating_sub(head_end);
    let tail_start = ceil_char_boundary(&content, content.len().saturating_sub(tail_bytes));
    format!("{}{marker}{}", &content[..head_end], &content[tail_start..])
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    const FAKE_SERVER: &str = r#"
import json
import os
import sys
import time

log_path = os.environ["MCP_TEST_LOG"]

def read_message():
    line = sys.stdin.readline()
    return json.loads(line) if line else None

def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method", "")
    with open(log_path, "a", encoding="utf-8") as log:
        log.write(method + "\n")
    if method == "initialize":
        send({"jsonrpc":"2.0", "id":message["id"], "result":{
            "protocolVersion":"2025-11-25",
            "capabilities":{"tools":{}, "resources":{}},
            "serverInfo":{"name":"fake", "version":"1"}
        }})
    elif method == "notifications/initialized":
        pass
    elif method == "ping":
        send({"jsonrpc":"2.0", "id":message["id"], "result":{}})
    elif method == "slow":
        time.sleep(0.3)
        send({"jsonrpc":"2.0", "id":message["id"], "result":{}})
    elif method == "tools/list":
        send({"jsonrpc":"2.0", "method":"notifications/tools/list_changed"})
        tool_name = "echo"
        if os.environ.get("MCP_TEST_DYNAMIC") == "1":
            with open(log_path, "r", encoding="utf-8") as log:
                if sum(1 for entry in log if entry.strip() == "tools/list") > 1:
                    tool_name = "echo_v2"
        send({"jsonrpc":"2.0", "id":message["id"], "result":{"tools":[{
            "name":tool_name,
            "description":"Echo an input value",
            "inputSchema":{"type":"object", "properties":{"value":{"type":"string"}}}
        }]}})
    elif method == "resources/list":
        send({"jsonrpc":"2.0", "id":message["id"], "result":{"resources":[{
            "uri":"memory://fake/guide",
            "name":"Fake guide",
            "mimeType":"text/markdown"
        }]}})
    elif method == "resources/read":
        uri = message.get("params", {}).get("uri", "")
        send({"jsonrpc":"2.0", "id":message["id"], "result":{"contents":[{
            "uri":uri,
            "mimeType":"text/markdown",
            "text":"Fake guide\nResource content"
        }]}})
    elif method == "tools/call":
        arguments = message.get("params", {}).get("arguments", {})
        if arguments.get("crash"):
            os._exit(3)
        if arguments.get("sleep"):
            time.sleep(0.3)
        send({"jsonrpc":"2.0", "id":message["id"], "result":{
            "content":[
                {"type":"text", "text":"echo:" + arguments.get("value", "")},
                {"type":"image", "mimeType":"image/png", "data":"AA=="}
            ],
            "isError":bool(arguments.get("error"))
        }})
"#;

    fn fake_config(temp: &TempDir) -> McpServerConfig {
        let script = temp.path().join("fake_mcp_server.py");
        fs::write(&script, FAKE_SERVER).expect("write fake MCP server");
        let log = temp.path().join("requests.log");
        McpServerConfig {
            name: "fake".into(),
            command: "python3".into(),
            args: vec![script.display().to_string()],
            envs: HashMap::from([("MCP_TEST_LOG".into(), log.display().to_string())]),
        }
    }

    #[tokio::test]
    async fn real_process_handshake_discovery_call_and_shutdown() {
        let temp = TempDir::new().expect("temp dir");
        let config = fake_config(&temp);
        let client = McpStdioClient::connect(&config, Duration::from_secs(2))
            .await
            .expect("connect");

        let tools = client.list_tools().await.expect("list tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "mcp__fake__echo");
        assert_eq!(tools[0].description(), "Echo an input value");
        assert_eq!(tools[0].input_schema().schema["type"], "object");
        let feature = DynamicToolSource::platform_feature(&client).expect("MCP health");
        assert_eq!(
            feature.status,
            sylvander_protocol::PlatformFeatureStatus::Active
        );
        assert!(feature.summary.contains("1 tools"));
        assert!(feature.summary.contains("1 resources"));
        assert!(feature.capabilities.contains(&"resources".to_owned()));
        client.probe_health().await.expect("health probe");

        let context = crate::tool_context::defaults::system_tool_context();
        let output = tools[0]
            .execute(&context, json!({ "value": "hello" }))
            .await
            .expect("call tool");
        assert!(!output.is_error);
        assert!(output.content.starts_with("echo:hello\n"));
        assert!(output.content.contains("\"type\":\"image\""));
        assert!(output.content.contains("<omitted 4 encoded bytes>"));
        assert!(!output.content.contains("AA=="));

        let model_error = tools[0]
            .execute(&context, json!({ "value": "no", "error": true }))
            .await
            .expect("model-visible tool error");
        assert!(model_error.is_error);
        assert!(model_error.content.starts_with("echo:no"));

        let resource_tools = client.resource_tools();
        assert_eq!(resource_tools.len(), 2);
        let list = resource_tools
            .iter()
            .find(|tool| tool.name().ends_with("__list_resources"))
            .expect("list resources tool");
        let listed = list
            .execute(&context, json!({}))
            .await
            .expect("list resources");
        assert!(listed.content.contains("memory://fake/guide"));
        let read = resource_tools
            .iter()
            .find(|tool| tool.name().ends_with("__read_resource"))
            .expect("read resource tool");
        let resource_output = read
            .execute(&context, json!({ "uri": "memory://fake/guide" }))
            .await
            .expect("read resource");
        assert!(resource_output.content.contains("Resource content"));

        client.shutdown().await.expect("shutdown process");
        let log = fs::read_to_string(temp.path().join("requests.log")).expect("read request log");
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            [
                "initialize",
                "notifications/initialized",
                "tools/list",
                "resources/list",
                "ping",
                "tools/call",
                "tools/call",
                "resources/list",
                "resources/read"
            ]
        );
    }

    #[tokio::test]
    async fn tool_call_timeout_is_reported_and_process_can_be_stopped() {
        let temp = TempDir::new().expect("temp dir");
        let config = fake_config(&temp);
        let timeout = Duration::from_millis(200);
        let client = McpStdioClient::connect(&config, timeout)
            .await
            .expect("connect");
        let tool = client.list_tools().await.expect("list tools").remove(0);
        let context = crate::tool_context::defaults::system_tool_context();

        let error = tool
            .execute(&context, json!({ "sleep": true }))
            .await
            .expect_err("slow call must time out");
        assert!(matches!(error, ToolError::Timeout(duration) if duration == timeout));
        client.shutdown().await.expect("shutdown after timeout");
    }

    #[tokio::test]
    async fn timeout_and_dropped_request_emit_protocol_cancellation() {
        let temp = TempDir::new().expect("temp dir");
        let config = fake_config(&temp);
        let client = McpStdioClient::connect(&config, Duration::from_millis(100))
            .await
            .expect("connect");

        let error = client
            .request("slow", json!({}))
            .await
            .expect_err("slow request must time out");
        assert!(matches!(error, McpError::Timeout { .. }));
        tokio::time::sleep(Duration::from_millis(300)).await;

        let interrupted_client = client.clone();
        let interrupted =
            tokio::spawn(async move { interrupted_client.request("slow", json!({})).await });
        tokio::time::sleep(Duration::from_millis(25)).await;
        interrupted.abort();
        assert!(interrupted.await.unwrap_err().is_cancelled());
        tokio::time::sleep(Duration::from_millis(400)).await;

        let feature = DynamicToolSource::platform_feature(&client).expect("MCP health");
        assert!(feature.summary.contains("2 cancellations"));
        client.shutdown().await.expect("shutdown process");
        let log = fs::read_to_string(temp.path().join("requests.log")).unwrap();
        assert_eq!(
            log.lines()
                .filter(|method| *method == "notifications/cancelled")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn transport_failure_reconnects_for_the_next_tool_call_without_replaying_it() {
        let temp = TempDir::new().expect("temp dir");
        let config = fake_config(&temp);
        let client = McpStdioClient::connect(&config, Duration::from_secs(2))
            .await
            .expect("connect");
        let tool = client.list_tools().await.expect("list tools").remove(0);
        let context = crate::tool_context::defaults::system_tool_context();

        let error = tool
            .execute(&context, json!({ "crash": true }))
            .await
            .expect_err("crashed process must fail the in-flight call");
        assert!(matches!(error, ToolError::Other(_)));
        let recovered = tool
            .execute(&context, json!({ "value": "after-reconnect" }))
            .await
            .expect("the next call uses the replacement process");
        assert_eq!(
            recovered.content.lines().next(),
            Some("echo:after-reconnect")
        );

        client
            .shutdown()
            .await
            .expect("shutdown replacement process");
        let log = fs::read_to_string(temp.path().join("requests.log")).unwrap();
        assert_eq!(
            log.lines().filter(|method| *method == "initialize").count(),
            2
        );
        assert_eq!(
            log.lines().filter(|method| *method == "tools/call").count(),
            2
        );
    }

    #[tokio::test]
    async fn reconnect_atomically_refreshes_the_dynamic_tool_catalog() {
        let temp = TempDir::new().expect("temp dir");
        let mut config = fake_config(&temp);
        config.envs.insert("MCP_TEST_DYNAMIC".into(), "1".into());
        let client = McpStdioClient::connect(&config, Duration::from_secs(2))
            .await
            .expect("connect");
        client.list_tools().await.expect("initial discovery");
        let registry = crate::tool::ToolRegistry::new().register_dynamic_source(client.clone());
        assert!(registry.get("mcp__fake__echo").is_some());
        assert!(registry.get("mcp__fake__echo_v2").is_none());

        let tool = registry.get("mcp__fake__echo").expect("initial tool");
        let context = crate::tool_context::defaults::system_tool_context();
        tool.execute(&context, json!({ "crash": true }))
            .await
            .expect_err("crashed call triggers reconnect");

        assert!(registry.get("mcp__fake__echo").is_none());
        assert!(registry.get("mcp__fake__echo_v2").is_some());
        let names = registry
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "mcp__fake__echo_v2",
                "mcp__fake__list_resources",
                "mcp__fake__read_resource"
            ]
        );
        let feature = DynamicToolSource::platform_feature(&client).expect("MCP health");
        assert_eq!(
            feature.status,
            sylvander_protocol::PlatformFeatureStatus::Active
        );
        assert!(feature.summary.contains("generation 2"));
        assert!(feature.summary.contains("1 reconnects"));

        client.shutdown().await.expect("shutdown replacement");
        assert_eq!(
            DynamicToolSource::platform_feature(&client)
                .expect("MCP health")
                .status,
            sylvander_protocol::PlatformFeatureStatus::Unavailable
        );
    }

    #[test]
    fn tool_results_keep_unicode_safe_head_and_tail_with_explicit_truncation() {
        let content = format!("{}TAIL-蟹", "前".repeat(MAX_TOOL_RESULT_BYTES));
        let output = map_tool_result(
            &json!({
                "content": [{ "type": "text", "text": content }],
                "isError": false
            }),
            None,
        );

        assert!(output.content.len() <= MAX_TOOL_RESULT_BYTES);
        assert!(output.content.starts_with('前'));
        assert!(output.content.contains("MCP result truncated"));
        assert!(output.content.ends_with("TAIL-蟹"));
    }

    #[test]
    fn public_tool_names_are_stable_bounded_and_mcp_namespaced() {
        assert_eq!(
            namespaced_tool_name("filesystem", "read_resource"),
            "mcp__filesystem__read_resource"
        );
        let transformed = namespaced_tool_name("本地 文件", "读取/资源");
        assert!(transformed.starts_with("mcp__"));
        assert!(transformed.len() <= 63);
        assert!(
            transformed.chars().all(
                |character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            )
        );
        assert_eq!(transformed, namespaced_tool_name("本地 文件", "读取/资源"));
        assert_ne!(
            namespaced_tool_name("server a", "read"),
            namespaced_tool_name("server/a", "read")
        );
    }

    #[tokio::test]
    async fn complete_results_are_persisted_but_presented_as_bounded_summaries() {
        let directory = tempfile::tempdir().expect("tempdir");
        let result = json!({
            "content": [{
                "type": "text",
                "text": format!("{}TAIL", "x".repeat(MAX_TOOL_RESULT_BYTES))
            }],
            "structuredContent": {
                "kept": true
            }
        });

        let path = persist_result_artifact(
            directory.path(),
            "session/one",
            "search server",
            "lookup",
            &result,
        )
        .await
        .expect("persist result");
        let output = map_tool_result(&result, Some(&path));

        assert!(path.starts_with(directory.path().join("session_one/search_server")));
        assert_eq!(
            serde_json::from_slice::<JsonValue>(&tokio::fs::read(&path).await.unwrap()).unwrap(),
            result
        );
        assert!(output.content.contains("MCP result truncated"));
        assert!(output.content.contains("Full result artifact:"));
        assert!(output.content.len() <= MAX_TOOL_RESULT_BYTES);
    }
}
