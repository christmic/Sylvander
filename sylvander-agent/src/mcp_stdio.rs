//! Minimal MCP stdio transport and [`Tool`](crate::tool::Tool) adapter.
//!
//! The transport owns one server process and serializes JSON-RPC requests over
//! newline-delimited JSON-RPC on stdin/stdout. Composition code can connect once,
//! discover the tools, and register the returned [`McpTool`] values in the
//! ordinary [`ToolRegistry`](crate::tool::ToolRegistry).

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value as JsonValue, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

use sylvander_llm_anthropic::api::types::InputSchema;

use crate::spec::McpServerConfig;
use crate::tool::{Tool, ToolError, ToolOutput};
use crate::tool_context::ToolContext;

const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_TOOL_RESULT_BYTES: usize = 64 * 1024;
const TOOL_RESULT_HEAD_BYTES: usize = 16 * 1024;

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
    request_timeout: Duration,
    next_id: AtomicU64,
    io: Mutex<ProcessIo>,
    child: Mutex<Child>,
}

/// A connected MCP stdio server.
#[derive(Clone)]
pub struct McpStdioClient {
    inner: Arc<McpInner>,
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
                request_timeout,
                next_id: AtomicU64::new(1),
                io: Mutex::new(ProcessIo {
                    stdin,
                    stdout: BufReader::new(stdout),
                }),
                child: Mutex::new(child),
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
        client
            .notify("notifications/initialized", json!({}))
            .await?;
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

        tools
            .iter()
            .map(|definition| McpTool::from_definition(self.clone(), definition))
            .collect()
    }

    /// Stop the child process and wait for it to exit.
    pub async fn shutdown(&self) -> Result<(), McpError> {
        let mut child = self.inner.child.lock().await;
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(source) => return Err(self.io_error(source)),
        }
        child.kill().await.map_err(|source| self.io_error(source))?;
        child.wait().await.map_err(|source| self.io_error(source))?;
        Ok(())
    }

    async fn call_tool(&self, name: &str, arguments: JsonValue) -> Result<ToolOutput, McpError> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        Ok(map_tool_result(&result))
    }

    async fn request(&self, method: &str, params: JsonValue) -> Result<JsonValue, McpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let duration = self.inner.request_timeout;
        timeout(duration, self.request_inner(id, method, &request))
            .await
            .map_err(|_| McpError::Timeout {
                server: self.inner.server_name.clone(),
                method: method.into(),
                duration,
            })?
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

/// A discovered MCP tool adapted to Sylvander's ordinary tool interface.
#[derive(Debug, Clone)]
pub struct McpTool {
    client: McpStdioClient,
    name: String,
    description: String,
    input_schema: InputSchema,
}

impl McpTool {
    fn from_definition(client: McpStdioClient, definition: &JsonValue) -> Result<Self, McpError> {
        let server = client.inner.server_name.clone();
        let name = definition
            .get("name")
            .and_then(JsonValue::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| McpError::InvalidResult {
                server: server.clone(),
                method: "tools/list".into(),
                message: "tool is missing a name".into(),
            })?
            .to_owned();
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

    async fn execute(&self, _ctx: &ToolContext, input: JsonValue) -> Result<ToolOutput, ToolError> {
        self.client
            .call_tool(&self.name, input)
            .await
            .map_err(|error| match error {
                McpError::Timeout { duration, .. } => ToolError::Timeout(duration),
                other => ToolError::Other(other.to_string()),
            })
    }
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

fn map_tool_result(result: &JsonValue) -> ToolOutput {
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
    let content = bound_tool_result(parts.join("\n"));
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
    if content.len() <= MAX_TOOL_RESULT_BYTES {
        return content;
    }
    let marker = format!(
        "\n… MCP result truncated: {} bytes total …\n",
        content.len()
    );
    let available = MAX_TOOL_RESULT_BYTES.saturating_sub(marker.len());
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
            "capabilities":{"tools":{}},
            "serverInfo":{"name":"fake", "version":"1"}
        }})
    elif method == "notifications/initialized":
        pass
    elif method == "tools/list":
        send({"jsonrpc":"2.0", "method":"notifications/tools/list_changed"})
        send({"jsonrpc":"2.0", "id":message["id"], "result":{"tools":[{
            "name":"echo",
            "description":"Echo an input value",
            "inputSchema":{"type":"object", "properties":{"value":{"type":"string"}}}
        }]}})
    elif method == "tools/call":
        arguments = message.get("params", {}).get("arguments", {})
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
        assert_eq!(tools[0].name(), "echo");
        assert_eq!(tools[0].description(), "Echo an input value");
        assert_eq!(tools[0].input_schema().schema["type"], "object");

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

        client.shutdown().await.expect("shutdown process");
        let log = fs::read_to_string(temp.path().join("requests.log")).expect("read request log");
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            [
                "initialize",
                "notifications/initialized",
                "tools/list",
                "tools/call",
                "tools/call"
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

    #[test]
    fn tool_results_keep_unicode_safe_head_and_tail_with_explicit_truncation() {
        let content = format!("{}TAIL-蟹", "前".repeat(MAX_TOOL_RESULT_BYTES));
        let output = map_tool_result(&json!({
            "content": [{ "type": "text", "text": content }],
            "isError": false
        }));

        assert!(output.content.len() <= MAX_TOOL_RESULT_BYTES);
        assert!(output.content.starts_with('前'));
        assert!(output.content.contains("MCP result truncated"));
        assert!(output.content.ends_with("TAIL-蟹"));
    }
}
