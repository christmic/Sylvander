//! Runtime-owned MCP Streamable HTTP transport.
//!
//! This supports the current single-endpoint transport. SSE is accepted only
//! as a response representation; legacy two-endpoint HTTP+SSE is unsupported.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use rmcp::model::{CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation};
use rmcp::service::RunningService;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value as JsonValue;
use thiserror::Error;
use tokio::sync::Mutex;
use url::Url;

use crate::agent_definition::McpStreamableHttpConfig;
use crate::mcp::stdio::{
    McpResultArtifact, McpResultArtifactSink, map_tool_result, namespaced_tool_name,
};
use sylvander_agent::tool::invocation::ToolInvocationClass;
use sylvander_agent::tool::{
    DynamicToolSource, PreparedToolCall, RegisteredTool, ToolDefinition, ToolError, ToolExecutor,
    ToolOutput, ToolSourceFeature, ToolSourceKind, ToolSourceStatus, ToolSpec,
};
use sylvander_agent::tool_context::ToolContext;

const MCP_HEALTH_ACTIVE: u8 = 1;
const MCP_HEALTH_DEGRADED: u8 = 2;
const MCP_HEALTH_UNAVAILABLE: u8 = 3;
const MAX_TOOLS: usize = 4096;
const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

type HttpService = RunningService<RoleClient, ClientInfo>;

#[derive(Debug, Error)]
pub(crate) enum McpHttpError {
    #[error("MCP Streamable HTTP server {server} has an invalid endpoint")]
    InvalidEndpoint { server: String },
    #[error("MCP Streamable HTTP server {server} requires HTTPS")]
    InsecureEndpoint { server: String },
    #[error("MCP Streamable HTTP server {server} connection failed: {message}")]
    Connection { server: String, message: String },
    #[error("MCP Streamable HTTP server {server} request timed out")]
    Timeout { server: String },
    #[error("MCP Streamable HTTP server {server} returned an invalid result")]
    InvalidResult { server: String },
}

struct McpHttpInner {
    server_name: String,
    endpoint: String,
    authenticated: bool,
    service: Mutex<HttpService>,
    tools: std::sync::RwLock<Vec<HttpMcpTool>>,
    result_artifact_sink: Option<Arc<dyn McpResultArtifactSink>>,
    health: AtomicU8,
}

#[derive(Clone)]
pub(crate) struct McpStreamableHttpClient {
    inner: Arc<McpHttpInner>,
}

impl std::fmt::Debug for McpStreamableHttpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpStreamableHttpClient")
            .field("server_name", &self.inner.server_name)
            .field("endpoint", &self.inner.endpoint)
            .field("authenticated", &self.inner.authenticated)
            .finish_non_exhaustive()
    }
}

impl McpStreamableHttpClient {
    pub(crate) async fn connect(
        config: &McpStreamableHttpConfig,
        bearer_token: Option<String>,
        result_artifact_sink: Option<Arc<dyn McpResultArtifactSink>>,
    ) -> Result<Self, McpHttpError> {
        validate_endpoint(config)?;
        let mut transport_config =
            StreamableHttpClientTransportConfig::with_uri(config.url.clone());
        if let Some(token) = bearer_token.as_deref() {
            transport_config = transport_config.auth_header(token);
        }
        let http = reqwest::Client::builder()
            .https_only(!cfg!(test))
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| McpHttpError::Connection {
                server: config.name.clone(),
                message: error.to_string(),
            })?;
        let transport = StreamableHttpClientTransport::with_client(http, transport_config);
        let client_info = ClientInfo::new(
            ClientCapabilities::default(),
            Implementation::new("sylvander", env!("CARGO_PKG_VERSION")),
        );
        let service = tokio::time::timeout(REQUEST_TIMEOUT, client_info.serve(transport))
            .await
            .map_err(|_| McpHttpError::Timeout {
                server: config.name.clone(),
            })?
            .map_err(|error| McpHttpError::Connection {
                server: config.name.clone(),
                message: error.to_string(),
            })?;
        let client = Self {
            inner: Arc::new(McpHttpInner {
                server_name: config.name.clone(),
                endpoint: config.url.clone(),
                authenticated: bearer_token.is_some(),
                service: Mutex::new(service),
                tools: std::sync::RwLock::new(Vec::new()),
                result_artifact_sink,
                health: AtomicU8::new(MCP_HEALTH_ACTIVE),
            }),
        };
        client.refresh_tools().await?;
        Ok(client)
    }

    pub(crate) async fn refresh_tools(&self) -> Result<(), McpHttpError> {
        let service = self.inner.service.lock().await;
        let tools = tokio::time::timeout(REQUEST_TIMEOUT, service.list_all_tools())
            .await
            .map_err(|_| self.timeout())?
            .map_err(|error| self.connection(error))?;
        drop(service);
        if tools.len() > MAX_TOOLS {
            return Err(McpHttpError::InvalidResult {
                server: self.inner.server_name.clone(),
            });
        }
        let tools = tools
            .into_iter()
            .map(|tool| HttpMcpTool::from_remote(self.clone(), tool))
            .collect::<Result<Vec<_>, _>>()?;
        *self
            .inner
            .tools
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = tools;
        self.inner
            .health
            .store(MCP_HEALTH_ACTIVE, Ordering::Release);
        Ok(())
    }

    pub(crate) async fn shutdown(&self) {
        let mut service = self.inner.service.lock().await;
        let _ = service.close_with_timeout(SHUTDOWN_TIMEOUT).await;
        self.inner
            .health
            .store(MCP_HEALTH_UNAVAILABLE, Ordering::Release);
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: JsonValue,
        user_id: &str,
        session_id: &str,
    ) -> Result<ToolOutput, McpHttpError> {
        let object = arguments
            .as_object()
            .cloned()
            .ok_or_else(|| McpHttpError::InvalidResult {
                server: self.inner.server_name.clone(),
            })?;
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(object);
        let service = self.inner.service.lock().await;
        let result = tokio::time::timeout(REQUEST_TIMEOUT, service.call_tool(params))
            .await
            .map_err(|_| self.timeout())?
            .map_err(|error| self.connection(error))?;
        drop(service);
        let result = serde_json::to_value(result).map_err(|_| McpHttpError::InvalidResult {
            server: self.inner.server_name.clone(),
        })?;
        let locator = self
            .persist_result_artifact(user_id, session_id, name, &result)
            .await;
        self.inner
            .health
            .store(MCP_HEALTH_ACTIVE, Ordering::Release);
        Ok(map_tool_result(&result, locator.as_deref()))
    }

    async fn persist_result_artifact(
        &self,
        user_id: &str,
        session_id: &str,
        operation: &str,
        result: &JsonValue,
    ) -> Option<String> {
        let sink = self.inner.result_artifact_sink.as_ref()?;
        let payload = serde_json::to_vec(result).ok()?;
        if payload.len() <= MAX_TOOL_RESULT_BYTES {
            return None;
        }
        sink.persist(McpResultArtifact {
            user_id: user_id.to_owned(),
            session_id: session_id.to_owned(),
            server: self.inner.server_name.clone(),
            operation: operation.to_owned(),
            media_type: "application/json".into(),
            payload,
            created_at: crate::session::now_secs(),
        })
        .await
        .ok()
    }

    fn timeout(&self) -> McpHttpError {
        self.inner
            .health
            .store(MCP_HEALTH_DEGRADED, Ordering::Release);
        McpHttpError::Timeout {
            server: self.inner.server_name.clone(),
        }
    }

    fn connection(&self, error: impl std::fmt::Display) -> McpHttpError {
        self.inner
            .health
            .store(MCP_HEALTH_DEGRADED, Ordering::Release);
        McpHttpError::Connection {
            server: self.inner.server_name.clone(),
            message: error.to_string(),
        }
    }
}

impl DynamicToolSource for McpStreamableHttpClient {
    fn snapshot(&self) -> Vec<Arc<dyn RegisteredTool>> {
        self.inner
            .tools
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .map(|tool| Arc::new(tool) as Arc<dyn RegisteredTool>)
            .collect()
    }

    fn platform_feature(&self) -> Option<ToolSourceFeature> {
        let status = match self.inner.health.load(Ordering::Acquire) {
            MCP_HEALTH_ACTIVE => ToolSourceStatus::Active,
            MCP_HEALTH_DEGRADED => ToolSourceStatus::Degraded,
            _ => ToolSourceStatus::Unavailable,
        };
        Some(ToolSourceFeature {
            kind: ToolSourceKind::Mcp,
            name: self.inner.server_name.clone(),
            status,
            summary: format!(
                "{} tools · Streamable HTTP",
                self.inner
                    .tools
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .len()
            ),
            source: Some(self.inner.endpoint.clone()),
            requires_authentication: self.inner.authenticated,
            capabilities: vec!["tools".into(), "streamable_http".into()],
            reloadable: true,
        })
    }
}

#[derive(Debug, Clone)]
struct HttpMcpTool {
    client: McpStreamableHttpClient,
    remote_name: String,
    name: String,
    description: String,
    input_schema: JsonValue,
}

impl HttpMcpTool {
    fn from_remote(
        client: McpStreamableHttpClient,
        tool: rmcp::model::Tool,
    ) -> Result<Self, McpHttpError> {
        let remote_name = tool.name.into_owned();
        if remote_name.trim().is_empty() {
            return Err(McpHttpError::InvalidResult {
                server: client.inner.server_name.clone(),
            });
        }
        Ok(Self {
            name: namespaced_tool_name(&client.inner.server_name, &remote_name),
            remote_name,
            description: tool
                .description
                .map_or_else(|| "MCP tool".into(), std::borrow::Cow::into_owned),
            input_schema: JsonValue::Object((*tool.input_schema).clone()),
            client,
        })
    }
}

impl ToolDefinition for HttpMcpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::immediate(
            self.name.clone(),
            self.description.clone(),
            self.input_schema.clone(),
            ToolInvocationClass::ArbitraryMcp,
        )
    }
}

#[async_trait]
impl ToolExecutor for HttpMcpTool {
    async fn handle(
        &self,
        context: &ToolContext,
        call: &PreparedToolCall,
    ) -> Result<ToolOutput, ToolError> {
        self.client
            .call_tool(
                &self.remote_name,
                call.input().clone(),
                context.user_id(),
                context.session_id(),
            )
            .await
            .map_err(|error| match error {
                McpHttpError::Timeout { .. } => ToolError::Timeout(REQUEST_TIMEOUT),
                other => ToolError::Other(other.to_string()),
            })
    }
}

fn validate_endpoint(config: &McpStreamableHttpConfig) -> Result<(), McpHttpError> {
    let url = Url::parse(&config.url).map_err(|_| McpHttpError::InvalidEndpoint {
        server: config.name.clone(),
    })?;
    if url.scheme() != "https" && !(cfg!(test) && url.scheme() == "http") {
        return Err(McpHttpError::InsecureEndpoint {
            server: config.name.clone(),
        });
    }
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(McpHttpError::InvalidEndpoint {
            server: config.name.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/mcp_streamable_http.rs"]
mod tests;
