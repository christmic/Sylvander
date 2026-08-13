//! Runtime-owned MCP stdio transport and registered-tool adapter.
//!
//! The transport owns one server process and serializes JSON-RPC requests over
//! newline-delimited JSON-RPC on stdin/stdout. Runtime composition connects,
//! discovers tools, and registers the resulting implementations through
//! Agent's provider-neutral dynamic-tool contract. Process lifecycle,
//! reconnect, health, cancellation, and artifact persistence stay outside the
//! Agent kernel because they require concrete operating-system authority.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::timeout;

#[cfg(test)]
use std::process::Stdio;
#[cfg(test)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(test)]
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::agent_definition::McpServerConfig;
use crate::execution::{
    PersistentProcess, PersistentProcessAuthority, PersistentProcessEnvironment,
    PersistentProcessError, PersistentProcessSpec,
};
use sylvander_agent::tool::invocation::ToolInvocationClass;
use sylvander_agent::tool::{
    DynamicToolSource, PreparedToolCall, RegisteredTool, ToolDefinition, ToolError, ToolExecutor,
    ToolOutput, ToolSourceFeature, ToolSourceKind, ToolSourceStatus, ToolSpec,
};
use sylvander_agent::tool_context::ToolContext;
use sylvander_llm_core::InputSchema;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOOL_PAGES: usize = 32;
const MAX_TOOLS: usize = 4096;
const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;
const TOOL_RESULT_HEAD_BYTES: usize = 16 * 1024;
const MCP_HEALTH_ACTIVE: u8 = 1;
const MCP_HEALTH_DEGRADED: u8 = 2;
const MCP_HEALTH_UNAVAILABLE: u8 = 3;
const MCP_HEALTH_INTERVAL: Duration = Duration::from_secs(30);

/// Complete MCP result handed to a Runtime-owned governed artifact store.
#[derive(Debug, Clone)]
pub(crate) struct McpResultArtifact {
    pub(crate) user_id: String,
    pub(crate) session_id: String,
    pub(crate) server: String,
    pub(crate) operation: String,
    pub(crate) media_type: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) created_at: i64,
}

/// Storage boundary for full MCP results. Implementations return an opaque,
/// user-safe locator rather than a host filesystem path.
#[async_trait]
pub(crate) trait McpResultArtifactSink: Send + Sync {
    async fn persist(&self, artifact: McpResultArtifact) -> Result<String, String>;
}

/// Errors raised while starting or communicating with an MCP server.
#[derive(Debug, Error)]
pub(crate) enum McpError {
    #[error("MCP server {server} process boundary failed: {source}")]
    Process {
        server: String,
        #[source]
        source: PersistentProcessError,
    },
    #[error("MCP server {server} closed its output")]
    Closed { server: String },
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
    #[error("MCP server {server} connection generation changed from {expected} to {actual}")]
    StaleGeneration {
        server: String,
        expected: u64,
        actual: u64,
    },
}

#[derive(Clone)]
enum McpProcessFactory {
    Managed {
        environment: Arc<dyn PersistentProcessEnvironment>,
        authority: PersistentProcessAuthority,
    },
    #[cfg(test)]
    TestHost,
}

impl McpProcessFactory {
    fn drain_timeout(&self) -> Duration {
        match self {
            Self::Managed { authority, .. } => authority.drain_timeout,
            #[cfg(test)]
            Self::TestHost => Duration::from_secs(2),
        }
    }
}

struct McpInner {
    server_name: String,
    config: McpServerConfig,
    request_timeout: Duration,
    next_id: AtomicU64,
    generation: AtomicU64,
    reconnect: Mutex<()>,
    process_factory: McpProcessFactory,
    process: Mutex<Box<dyn PersistentProcess>>,
    result_artifact_sink: Option<Arc<dyn McpResultArtifactSink>>,
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
pub(crate) struct McpStdioClient {
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
    /// Start a server inside one admitted persistent-process environment.
    pub(crate) async fn connect_in(
        config: &McpServerConfig,
        request_timeout: Duration,
        environment: Arc<dyn PersistentProcessEnvironment>,
        authority: PersistentProcessAuthority,
    ) -> Result<Self, McpError> {
        if !environment.isolation().enforces_required_boundary() {
            return Err(McpError::Process {
                server: config.name.clone(),
                source: PersistentProcessError::InvalidAuthority(
                    "execution environment does not enforce the required boundary",
                ),
            });
        }
        Self::connect_inner(
            config,
            request_timeout,
            None,
            true,
            McpProcessFactory::Managed {
                environment,
                authority,
            },
        )
        .await
    }

    /// Test-only host process path for exercising protocol mechanics. Product
    /// composition has no call surface for unconfined process execution.
    #[cfg(test)]
    async fn connect(
        config: &McpServerConfig,
        request_timeout: Duration,
    ) -> Result<Self, McpError> {
        Self::connect_inner(
            config,
            request_timeout,
            None,
            true,
            McpProcessFactory::TestHost,
        )
        .await
    }

    /// Start a server and persist every complete tool result through `sink`.
    ///
    /// Callers still receive a bounded summary. The durable JSON artifact is
    /// retained for later inspection, debugging, and evidence-driven
    /// improvement without flooding the model or UI.
    pub(crate) async fn connect_with_result_artifact_sink(
        config: &McpServerConfig,
        request_timeout: Duration,
        sink: Arc<dyn McpResultArtifactSink>,
        environment: Arc<dyn PersistentProcessEnvironment>,
        authority: PersistentProcessAuthority,
    ) -> Result<Self, McpError> {
        if !environment.isolation().enforces_required_boundary() {
            return Err(McpError::Process {
                server: config.name.clone(),
                source: PersistentProcessError::InvalidAuthority(
                    "execution environment does not enforce the required boundary",
                ),
            });
        }
        Self::connect_inner(
            config,
            request_timeout,
            Some(sink),
            true,
            McpProcessFactory::Managed {
                environment,
                authority,
            },
        )
        .await
    }

    async fn connect_inner(
        config: &McpServerConfig,
        request_timeout: Duration,
        result_artifact_sink: Option<Arc<dyn McpResultArtifactSink>>,
        start_health_monitor: bool,
        process_factory: McpProcessFactory,
    ) -> Result<Self, McpError> {
        let process = spawn_process(config, &process_factory).await?;

        let client = Self {
            inner: Arc::new(McpInner {
                server_name: config.name.clone(),
                config: config.clone(),
                request_timeout,
                next_id: AtomicU64::new(1),
                generation: AtomicU64::new(1),
                reconnect: Mutex::new(()),
                process_factory,
                process: Mutex::new(process),
                result_artifact_sink,
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
    pub(crate) async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let mut definitions = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |cursor| json!({ "cursor": cursor }));
            let result = self.request("tools/list", params).await?;
            let page = result
                .get("tools")
                .and_then(JsonValue::as_array)
                .ok_or_else(|| McpError::InvalidResult {
                    server: self.inner.server_name.clone(),
                    method: "tools/list".into(),
                    message: "missing tools array".into(),
                })?;
            if definitions.len().saturating_add(page.len()) > MAX_TOOLS {
                return Err(McpError::InvalidResult {
                    server: self.inner.server_name.clone(),
                    method: "tools/list".into(),
                    message: format!("tool catalog exceeds {MAX_TOOLS} entries"),
                });
            }
            definitions.extend(page.iter().cloned());
            let next = result
                .get("nextCursor")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
            if next.is_none() {
                cursor = None;
                break;
            }
            if next == cursor {
                return Err(McpError::InvalidResult {
                    server: self.inner.server_name.clone(),
                    method: "tools/list".into(),
                    message: "tool pagination cursor did not advance".into(),
                });
            }
            cursor = next;
        }
        if cursor.is_some() {
            return Err(McpError::InvalidResult {
                server: self.inner.server_name.clone(),
                method: "tools/list".into(),
                message: format!("tool catalog exceeds {MAX_TOOL_PAGES} pages"),
            });
        }

        let discovered = definitions
            .iter()
            .map(|definition| McpTool::from_definition(self.clone(), definition))
            .collect::<Result<Vec<_>, _>>()?;
        self.inner
            .tool_definitions
            .write()
            .unwrap()
            .clone_from(&definitions);
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

    fn resource_tools(&self) -> Vec<Arc<dyn RegisteredTool>> {
        if !self.inner.supports_resources.load(Ordering::Acquire) {
            return Vec::new();
        }
        [McpResourceOperation::List, McpResourceOperation::Read]
            .into_iter()
            .map(|operation| {
                Arc::new(McpResourceTool::new(self.clone(), operation)) as Arc<dyn RegisteredTool>
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

    async fn read_resource(
        &self,
        uri: &str,
        user_id: &str,
        session_id: &str,
    ) -> Result<ToolOutput, McpError> {
        let result = self
            .request("resources/read", json!({ "uri": uri }))
            .await?;
        let locator = self
            .persist_result_artifact(user_id, session_id, "read_resource", &result)
            .await;
        Ok(map_tool_result(&result, locator.as_deref()))
    }

    /// Stop the complete governed process tree.
    pub(crate) async fn shutdown(&self) -> Result<(), McpError> {
        self.inner.shutdown.store(true, Ordering::Release);
        let mut process = self.inner.process.lock().await;
        let drain_timeout = self.inner.process_factory.drain_timeout();
        let result =
            if process.close_stdin().await.is_ok() && process.wait(drain_timeout).await.is_ok() {
                Ok(())
            } else {
                process
                    .terminate_tree()
                    .await
                    .map_err(|source| self.process_error(source))
            };
        self.inner
            .health
            .store(MCP_HEALTH_UNAVAILABLE, Ordering::Release);
        result
    }

    /// Probe the MCP transport without exposing server content.
    pub(crate) async fn probe_health(&self) -> Result<(), McpError> {
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
        user_id: &str,
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
                let locator = self
                    .persist_result_artifact(user_id, session_id, name, &result)
                    .await;
                Ok(map_tool_result(&result, locator.as_deref()))
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

    fn ensure_generation(&self, expected: u64) -> Result<(), McpError> {
        let actual = self.inner.generation.load(Ordering::Acquire);
        if actual == expected {
            Ok(())
        } else {
            Err(McpError::StaleGeneration {
                server: self.inner.server_name.clone(),
                expected,
                actual,
            })
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
            self.inner.result_artifact_sink.clone(),
            false,
            self.inner.process_factory.clone(),
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
        let new_process = replacement.process.into_inner();

        let mut process = self.inner.process.lock().await;
        process
            .terminate_tree()
            .await
            .map_err(|source| self.process_error(source))?;
        *process = new_process;
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

    async fn persist_result_artifact(
        &self,
        user_id: &str,
        session_id: &str,
        operation: &str,
        result: &JsonValue,
    ) -> Option<String> {
        let sink = self.inner.result_artifact_sink.as_ref()?;
        let payload =
            serde_json::to_vec(result).expect("serializing an MCP JSON result cannot fail");
        match sink
            .persist(McpResultArtifact {
                user_id: user_id.to_owned(),
                session_id: session_id.to_owned(),
                server: self.inner.server_name.clone(),
                operation: operation.to_owned(),
                media_type: "application/json".into(),
                payload,
                created_at: crate::session::now_secs(),
            })
            .await
        {
            Ok(locator) => Some(locator),
            Err(error) => {
                tracing::warn!(
                    server = %self.inner.server_name,
                    operation,
                    session_id,
                    error,
                    "failed to persist governed MCP result artifact"
                );
                None
            }
        }
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
        let mut process = self.inner.process.lock().await;
        write_process_frame(process.as_mut(), request)
            .await
            .map_err(|source| self.process_error(source))?;

        loop {
            let response = read_process_frame(process.as_mut(), &self.inner.server_name).await?;
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
        let mut process = self.inner.process.lock().await;
        write_process_frame(process.as_mut(), &notification)
            .await
            .map_err(|source| self.process_error(source))
    }

    fn process_error(&self, source: PersistentProcessError) -> McpError {
        McpError::Process {
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
    fn snapshot(&self) -> Vec<Arc<dyn RegisteredTool>> {
        let mut tools = self
            .current_tools()
            .into_iter()
            .map(|tool| Arc::new(tool) as Arc<dyn RegisteredTool>)
            .collect::<Vec<_>>();
        tools.extend(self.resource_tools());
        tools
    }

    fn platform_feature(&self) -> Option<ToolSourceFeature> {
        let status = match self.inner.health.load(Ordering::Acquire) {
            MCP_HEALTH_ACTIVE => ToolSourceStatus::Active,
            MCP_HEALTH_DEGRADED => ToolSourceStatus::Degraded,
            _ => ToolSourceStatus::Unavailable,
        };
        let tool_count = self.inner.tool_definitions.read().unwrap().len();
        let resource_count = self.inner.resource_definitions.read().unwrap().len();
        let generation = self.inner.generation.load(Ordering::Acquire);
        let reconnects = self.inner.reconnect_count.load(Ordering::Acquire);
        let cancellations = self.inner.cancellation_count.load(Ordering::Acquire);
        Some(ToolSourceFeature {
            kind: ToolSourceKind::Mcp,
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
            requires_authentication: !self.inner.config.envs.is_empty(),
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
        McpError::Closed { .. } | McpError::Process { .. } | McpError::Timeout { .. }
    )
}

async fn spawn_process(
    config: &McpServerConfig,
    factory: &McpProcessFactory,
) -> Result<Box<dyn PersistentProcess>, McpError> {
    let spec = PersistentProcessSpec {
        program: config.command.clone(),
        arguments: config.args.clone(),
        environment: config
            .envs
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>(),
    };
    let result = match factory {
        McpProcessFactory::Managed {
            environment,
            authority,
        } => environment.spawn(&spec, authority).await,
        #[cfg(test)]
        McpProcessFactory::TestHost => spawn_test_host(&spec),
    };
    result.map_err(|source| McpError::Process {
        server: config.name.clone(),
        source,
    })
}

#[cfg(test)]
struct TestHostPersistentProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

#[cfg(test)]
fn spawn_test_host(
    spec: &PersistentProcessSpec,
) -> Result<Box<dyn PersistentProcess>, PersistentProcessError> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.arguments)
        .env_clear()
        .envs(&spec.environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or(PersistentProcessError::InvalidSpecification("test stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or(PersistentProcessError::InvalidSpecification("test stdout"))?;
    Ok(Box::new(TestHostPersistentProcess {
        child,
        stdin: Some(stdin),
        stdout: BufReader::new(stdout),
    }))
}

#[cfg(test)]
#[async_trait]
impl PersistentProcess for TestHostPersistentProcess {
    async fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), PersistentProcessError> {
        let stdin = self.stdin.as_mut().ok_or(PersistentProcessError::Closed)?;
        stdin.write_all(bytes).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn read_stdout_frame(&mut self) -> Result<Vec<u8>, PersistentProcessError> {
        let mut frame = Vec::new();
        let bytes = self.stdout.read_until(b'\n', &mut frame).await?;
        if bytes == 0 {
            return Err(PersistentProcessError::Closed);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(PersistentProcessError::FrameTooLarge(MAX_FRAME_BYTES));
        }
        Ok(frame)
    }

    async fn close_stdin(&mut self) -> Result<(), PersistentProcessError> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin.shutdown().await?;
        }
        Ok(())
    }

    async fn wait(&mut self, duration: Duration) -> Result<(), PersistentProcessError> {
        let status = timeout(duration, self.child.wait())
            .await
            .map_err(|_| PersistentProcessError::Timeout(duration))??;
        if status.success() {
            Ok(())
        } else {
            Err(PersistentProcessError::Exited(status.code()))
        }
    }

    async fn terminate_tree(&mut self) -> Result<(), PersistentProcessError> {
        self.stdin.take();
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }
        let _ = self.child.wait().await?;
        Ok(())
    }
}

/// A discovered MCP tool adapted to Sylvander's ordinary tool interface.
#[derive(Debug, Clone)]
pub(crate) struct McpTool {
    client: McpStdioClient,
    name: String,
    remote_name: String,
    description: String,
    input_schema: InputSchema,
    generation: u64,
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
        let input_schema =
            definition
                .get("inputSchema")
                .cloned()
                .ok_or_else(|| McpError::InvalidResult {
                    server: server.clone(),
                    method: "tools/list".into(),
                    message: format!("tool {name} is missing inputSchema"),
                })?;
        if !input_schema.is_object() {
            return Err(McpError::InvalidResult {
                server,
                method: "tools/list".into(),
                message: format!("tool {name} inputSchema is not an object"),
            });
        }
        Ok(Self {
            generation: client.inner.generation.load(Ordering::Acquire),
            client,
            name,
            remote_name,
            description,
            input_schema: InputSchema::from_json_value(input_schema),
        })
    }
}

impl ToolDefinition for McpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::immediate(
            self.name.clone(),
            self.description.clone(),
            self.input_schema.schema.clone(),
            ToolInvocationClass::ArbitraryMcp,
        )
    }
}

#[async_trait]
impl ToolExecutor for McpTool {
    async fn handle(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        self.client
            .ensure_generation(self.generation)
            .map_err(|error| ToolError::Other(error.to_string()))?;
        self.client
            .call_tool(
                &self.remote_name,
                call.input().clone(),
                ctx.user_id(),
                ctx.session_id(),
            )
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
    generation: u64,
}

impl McpResourceTool {
    fn new(client: McpStdioClient, operation: McpResourceOperation) -> Self {
        let remote_name = match operation {
            McpResourceOperation::List => "list_resources",
            McpResourceOperation::Read => "read_resource",
        };
        let name = namespaced_tool_name(&client.inner.server_name, remote_name);
        Self {
            generation: client.inner.generation.load(Ordering::Acquire),
            client,
            name,
            operation,
        }
    }
}

impl ToolDefinition for McpResourceTool {
    fn spec(&self) -> ToolSpec {
        let description = match self.operation {
            McpResourceOperation::List => "List resources currently advertised by this MCP server.",
            McpResourceOperation::Read => {
                "Read one MCP resource by its exact URI. Use list_resources first when needed."
            }
        };
        let input_schema = match self.operation {
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
        };
        ToolSpec::immediate(
            self.name.clone(),
            description,
            input_schema.schema,
            ToolInvocationClass::ArbitraryMcp,
        )
    }
}

#[async_trait]
impl ToolExecutor for McpResourceTool {
    async fn handle(
        &self,
        ctx: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        self.client
            .ensure_generation(self.generation)
            .map_err(|error| ToolError::Other(error.to_string()))?;
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
                let uri = call
                    .input()
                    .get("uri")
                    .and_then(JsonValue::as_str)
                    .filter(|uri| !uri.is_empty())
                    .ok_or_else(|| ToolError::Other("resource URI is required".into()))?;
                self.client
                    .read_resource(uri, ctx.user_id(), ctx.session_id())
                    .await
                    .map_err(|error| ToolError::Other(error.to_string()))
            }
        }
    }
}

pub(super) fn namespaced_tool_name(server: &str, remote_name: &str) -> String {
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

async fn write_process_frame(
    process: &mut dyn PersistentProcess,
    value: &JsonValue,
) -> Result<(), PersistentProcessError> {
    let mut body = serde_json::to_vec(value).expect("serializing JSON values cannot fail");
    body.push(b'\n');
    process.write_stdin(&body).await
}

async fn read_process_frame(
    process: &mut dyn PersistentProcess,
    server: &str,
) -> Result<JsonValue, McpError> {
    let mut line = process.read_stdout_frame().await.map_err(|source| {
        if matches!(source, PersistentProcessError::Closed) {
            McpError::Closed {
                server: server.into(),
            }
        } else {
            McpError::Process {
                server: server.into(),
                source,
            }
        }
    })?;
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

pub(super) fn map_tool_result(result: &JsonValue, artifact_locator: Option<&str>) -> ToolOutput {
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
    let content = match artifact_locator {
        Some(locator) => {
            let suffix = format!("\n\nFull result artifact: {locator}");
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
#[path = "../../tests/unit/mcp_stdio.rs"]
mod tests;
