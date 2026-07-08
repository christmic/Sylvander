//! Agent specification types — declarative agent description.
//!
//! The `AgentSpec` is a serializable declaration of what an agent IS:
//! its identity, personality, model preferences, tool set, and memory
//! configuration. It does NOT contain runtime state — that lives in
//! `AgentRun` (see `run.rs`).
//!
//! Two construction paths:
//! 1. **Programmatic**: `AgentSpec::builder()` — for embedding
//! 2. **TOML**: `toml::from_str::<AgentSpec>()` — for user-defined
//!    agents

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use sylvander_llm_anthropic::api::model::ModelInfo;

// ---------------------------------------------------------------------------
// ID types
// ---------------------------------------------------------------------------

/// Unique identifier for an agent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    /// Create a new `AgentId`.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for AgentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Unique identifier for a session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    /// Create a new `SessionId`.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Config sub-types
// ---------------------------------------------------------------------------

/// Agent personality — the "soul" of an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaConfig {
    /// System prompt injected at the start of every conversation.
    #[serde(default)]
    pub system_prompt: String,
    /// Human-readable description of the agent's role.
    #[serde(default)]
    pub description: String,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            description: String::new(),
        }
    }
}

/// Model selection and tuning parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Provider name (e.g. `"anthropic"`, `"openai"`, `"minimax"`).
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Model name / ID (e.g. `"claude-sonnet-5-20260601"`).
    #[serde(default)]
    pub model_name: String,
    /// Optional temperature override.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Optional max output tokens override.
    #[serde(default)]
    pub max_tokens: Option<u32>,
}

fn default_provider() -> String {
    "anthropic".to_string()
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model_name: String::new(),
            temperature: None,
            max_tokens: None,
        }
    }
}

/// Reference to a tool — either built-in or provided by an MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolRef {
    /// A built-in tool shipped with Sylvander (e.g. `"read"`, `"write"`).
    Builtin {
        /// The tool name (e.g. `"read"`, `"write"`, `"edit"`).
        name: String,
    },
    /// A tool provided by an external MCP server.
    McpServer(McpServerConfig),
}

/// Configuration for an MCP server that provides tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Human-readable name for this server.
    pub name: String,
    /// Shell command to start the server.
    pub command: String,
    /// CLI arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables passed to the server process.
    #[serde(default)]
    pub envs: HashMap<String, String>,
}

/// Configuration for a long-term memory store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryStoreConfig {
    /// Store type: `"in_memory"`, `"sqlite"`, etc.
    pub store_type: String,
    /// Path to the store file or directory.
    pub path: PathBuf,
}

impl MemoryStoreConfig {
    /// Resolve this config into an actual [`MemoryStore`] implementation.
    ///
    /// Currently supports `"in_memory"`. `"sqlite"` is planned.
    ///
    /// # Errors
    /// Returns an error for unknown store types.
    pub fn build(
        &self,
    ) -> Result<std::sync::Arc<dyn crate::tools::memory::MemoryStore>, crate::tools::memory::MemoryStoreError> {
        match self.store_type.as_str() {
            "in_memory" => Ok(std::sync::Arc::new(
                crate::tools::memory::InMemoryMemoryStore::new(),
            )),
            other => Err(crate::tools::memory::MemoryStoreError::Store(format!(
                "unknown memory store type: {other}"
            ))),
        }
    }
}

/// Behavior tuning parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorConfig {
    /// Maximum loop iterations before forced termination.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: u32,
    /// Maximum LLM call retries on transient errors.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

const fn default_max_iterations() -> u32 {
    50
}
const fn default_max_retries() -> u32 {
    3
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            max_retries: default_max_retries(),
        }
    }
}

// ---------------------------------------------------------------------------
// AgentSpec
// ---------------------------------------------------------------------------

/// Top-level declarative agent specification.
///
/// This is the serializable description of an agent — its identity,
/// personality, model, tool set, memory stores, and behavior. It can
/// be built programmatically via [`AgentSpecBuilder`] or deserialized
/// from TOML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSpec {
    /// Unique agent identifier.
    pub id: AgentId,
    /// Human-readable display name.
    pub name: String,
    /// Personality / system prompt configuration.
    #[serde(default)]
    pub persona: PersonaConfig,
    /// Model selection configuration.
    #[serde(default)]
    pub model: ModelConfig,
    /// Tool references (built-in + MCP).
    #[serde(default)]
    pub tools: Vec<ToolRef>,
    /// MCP server definitions (referenced by [`ToolRef::McpServer`]).
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    /// Long-term memory store configurations.
    #[serde(default)]
    pub memory_stores: Vec<MemoryStoreConfig>,
    /// Behavior tuning.
    #[serde(default)]
    pub behavior: BehaviorConfig,
}

impl AgentSpec {
    /// Start building an [`AgentSpec`].
    #[must_use]
    pub fn builder() -> AgentSpecBuilder {
        AgentSpecBuilder::default()
    }

    /// Convert the model config to a [`ModelInfo`].
    ///
    /// Uses a default 200k context window. Capabilities are left empty
    /// — callers should add them via `ModelInfo::builder().capability()`.
    #[must_use]
    pub fn to_model_info(&self) -> ModelInfo {
        let mut builder = ModelInfo::builder()
            .id(&self.model.model_name)
            .context_window(200_000);

        if let Some(max_tokens) = self.model.max_tokens {
            builder = builder.max_output_tokens(max_tokens);
        } else {
            builder = builder.max_output_tokens(32_000);
        }

        builder.build().unwrap_or_else(|| {
            panic!(
                "ModelInfo build failed for spec '{}': \
                 id/context_window/max_output_tokens required",
                self.id
            )
        })
    }
}

// ---------------------------------------------------------------------------
// AgentSpecBuilder
// ---------------------------------------------------------------------------

/// Builder for [`AgentSpec`].
///
/// Only `id` and `name` are required. All other fields have sensible
/// defaults (empty persona, default model, no tools, default behavior).
#[derive(Debug, Default)]
pub struct AgentSpecBuilder {
    id: Option<AgentId>,
    name: Option<String>,
    persona: PersonaConfig,
    model: ModelConfig,
    tools: Vec<ToolRef>,
    mcp_servers: Vec<McpServerConfig>,
    memory_stores: Vec<MemoryStoreConfig>,
    behavior: BehaviorConfig,
}

impl AgentSpecBuilder {
    // -- required --

    /// Set the agent ID (required).
    #[must_use]
    pub fn id(mut self, id: impl Into<AgentId>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Set the display name (required).
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    // -- persona --

    /// Set the system prompt.
    #[must_use]
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.persona.system_prompt = prompt.into();
        self
    }

    /// Set the description.
    #[must_use]
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.persona.description = desc.into();
        self
    }

    /// Set the entire persona config.
    #[must_use]
    pub fn persona(mut self, persona: PersonaConfig) -> Self {
        self.persona = persona;
        self
    }

    // -- model --

    /// Set the model config.
    #[must_use]
    pub fn model(mut self, model: ModelConfig) -> Self {
        self.model = model;
        self
    }

    /// Set the model name (convenience shortcut).
    #[must_use]
    pub fn model_name(mut self, name: impl Into<String>) -> Self {
        self.model.model_name = name.into();
        self
    }

    // -- tools --

    /// Register a built-in tool by name.
    #[must_use]
    pub fn builtin_tool(mut self, name: impl Into<String>) -> Self {
        self.tools.push(ToolRef::Builtin {
            name: name.into(),
        });
        self
    }

    /// Register an MCP server as a tool source.
    #[must_use]
    pub fn mcp_server(mut self, config: McpServerConfig) -> Self {
        self.tools.push(ToolRef::McpServer(config));
        self
    }

    /// Replace all tool references.
    #[must_use]
    pub fn tools(mut self, tools: Vec<ToolRef>) -> Self {
        self.tools = tools;
        self
    }

    /// Register an MCP server definition (does not auto-add to tools).
    #[must_use]
    pub fn mcp_server_def(mut self, config: McpServerConfig) -> Self {
        self.mcp_servers.push(config);
        self
    }

    // -- memory --

    /// Add a memory store configuration.
    #[must_use]
    pub fn memory_store(mut self, config: MemoryStoreConfig) -> Self {
        self.memory_stores.push(config);
        self
    }

    // -- behavior --

    /// Set the entire behavior config.
    #[must_use]
    pub fn behavior(mut self, behavior: BehaviorConfig) -> Self {
        self.behavior = behavior;
        self
    }

    /// Set max iterations.
    #[must_use]
    pub fn max_iterations(mut self, n: u32) -> Self {
        self.behavior.max_iterations = n;
        self
    }

    /// Set max retries.
    #[must_use]
    pub fn max_retries(mut self, n: u32) -> Self {
        self.behavior.max_retries = n;
        self
    }

    // -- build --

    /// Build the [`AgentSpec`].
    ///
    /// # Errors
    /// Returns [`AgentSpecError`] if required fields (`id`, `name`)
    /// are missing.
    pub fn build(self) -> Result<AgentSpec, AgentSpecError> {
        let id = self.id.ok_or(AgentSpecError::MissingId)?;
        let name = self.name.ok_or(AgentSpecError::MissingName)?;

        Ok(AgentSpec {
            id,
            name,
            persona: self.persona,
            model: self.model,
            tools: self.tools,
            mcp_servers: self.mcp_servers,
            memory_stores: self.memory_stores,
            behavior: self.behavior,
        })
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from building an [`AgentSpec`].
#[derive(Debug, thiserror::Error)]
pub enum AgentSpecError {
    /// The `id` field was not set.
    #[error("agent id is required")]
    MissingId,
    /// The `name` field was not set.
    #[error("agent name is required")]
    MissingName,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- builder --

    #[test]
    fn builder_basic() {
        let spec = AgentSpec::builder()
            .id("test-agent")
            .name("Test Agent")
            .system_prompt("You are a test agent.")
            .description("Used for testing")
            .model_name("claude-sonnet-5-20260601")
            .builtin_tool("read")
            .builtin_tool("write")
            .max_iterations(30)
            .build()
            .expect("build should succeed");

        assert_eq!(spec.id, AgentId::new("test-agent"));
        assert_eq!(spec.name, "Test Agent");
        assert_eq!(spec.persona.system_prompt, "You are a test agent.");
        assert_eq!(spec.persona.description, "Used for testing");
        assert_eq!(spec.model.model_name, "claude-sonnet-5-20260601");
        assert_eq!(spec.tools.len(), 2);
        assert!(matches!(&spec.tools[0], ToolRef::Builtin { name } if name == "read"));
        assert_eq!(spec.behavior.max_iterations, 30);
        assert_eq!(spec.behavior.max_retries, 3); // default
    }

    #[test]
    fn builder_missing_id() {
        let err = AgentSpec::builder()
            .name("No ID")
            .build()
            .unwrap_err();
        assert!(matches!(err, AgentSpecError::MissingId));
    }

    #[test]
    fn builder_missing_name() {
        let err = AgentSpec::builder()
            .id("no-name")
            .build()
            .unwrap_err();
        assert!(matches!(err, AgentSpecError::MissingName));
    }

    #[test]
    fn builder_defaults() {
        let spec = AgentSpec::builder()
            .id("minimal")
            .name("Minimal Agent")
            .build()
            .expect("build should succeed");

        assert!(spec.persona.system_prompt.is_empty());
        assert!(spec.tools.is_empty());
        assert!(spec.mcp_servers.is_empty());
        assert!(spec.memory_stores.is_empty());
        assert_eq!(spec.behavior.max_iterations, 50);
        assert_eq!(spec.behavior.max_retries, 3);
        assert_eq!(spec.model.provider, "anthropic");
    }

    // -- TOML --

    #[test]
    fn toml_roundtrip() {
        let spec = AgentSpec::builder()
            .id("toml-agent")
            .name("TOML Agent")
            .system_prompt("You are defined in TOML.")
            .model_name("claude-haiku-4-5-20251001")
            .builtin_tool("read")
            .build()
            .expect("build should succeed");

        let toml_str = toml::to_string_pretty(&spec).expect("serialize");
        let parsed: AgentSpec = toml::from_str(&toml_str).expect("deserialize");

        assert_eq!(parsed.id, spec.id);
        assert_eq!(parsed.name, spec.name);
        assert_eq!(parsed.persona.system_prompt, spec.persona.system_prompt);
        assert_eq!(parsed.model.model_name, spec.model.model_name);
        assert_eq!(parsed.tools.len(), spec.tools.len());
    }

    #[test]
    fn toml_deserialize_minimal() {
        let toml_str = r#"
id = "minimal-toml"
name = "Minimal TOML Agent"
"#;
        let spec: AgentSpec = toml::from_str(toml_str).expect("deserialize");
        assert_eq!(spec.id, AgentId::new("minimal-toml"));
        assert_eq!(spec.name, "Minimal TOML Agent");
        assert!(spec.persona.system_prompt.is_empty());
    }

    #[test]
    fn toml_deserialize_full() {
        let toml_str = r#"
id = "full-agent"
name = "Full Agent"

[persona]
system_prompt = "You are a helpful assistant."
description = "A fully configured agent"

[model]
provider = "anthropic"
model_name = "claude-sonnet-5-20260601"
temperature = 0.7
max_tokens = 4096

[[tools]]
type = "builtin"
name = "read"

[[tools]]
type = "builtin"
name = "write"

[[tools]]
type = "mcp_server"
name = "code-analyzer"
command = "code-analyzer-mcp"
args = ["--verbose"]

[[memory_stores]]
store_type = "sqlite"
path = "/tmp/agent-memory.db"

[behavior]
max_iterations = 30
max_retries = 5
"#;
        let spec: AgentSpec = toml::from_str(toml_str).expect("deserialize");

        assert_eq!(spec.id, AgentId::new("full-agent"));
        assert_eq!(spec.persona.system_prompt, "You are a helpful assistant.");
        assert_eq!(spec.model.temperature, Some(0.7));
        assert_eq!(spec.model.max_tokens, Some(4096));
        assert_eq!(spec.tools.len(), 3);
        assert_eq!(spec.memory_stores.len(), 1);
        assert_eq!(spec.memory_stores[0].store_type, "sqlite");
        assert_eq!(spec.behavior.max_iterations, 30);
        assert_eq!(spec.behavior.max_retries, 5);
    }

    // -- ModelInfo conversion --

    #[test]
    fn to_model_info() {
        let spec = AgentSpec::builder()
            .id("model-test")
            .name("Model Test")
            .model(ModelConfig {
                provider: "anthropic".into(),
                model_name: "claude-sonnet-5-20260601".into(),
                temperature: Some(0.5),
                max_tokens: Some(8192),
            })
            .build()
            .expect("build should succeed");

        let info = spec.to_model_info();
        assert_eq!(info.id, "claude-sonnet-5-20260601");
        assert_eq!(info.max_output_tokens, 8192);
        assert_eq!(info.context_window, 200_000);
    }

    #[test]
    fn to_model_info_default_max_tokens() {
        let spec = AgentSpec::builder()
            .id("default-tokens")
            .name("Default Tokens")
            .model_name("claude-opus-4-8")
            .build()
            .expect("build should succeed");

        let info = spec.to_model_info();
        assert_eq!(info.max_output_tokens, 32_000);
    }

    // -- ID types --

    #[test]
    fn agent_id_display() {
        let id = AgentId::new("test-123");
        assert_eq!(format!("{id}"), "test-123");
    }

    #[test]
    fn agent_id_from_str() {
        let id: AgentId = "from-str".into();
        assert_eq!(id.0, "from-str");
    }

    #[test]
    fn session_id_display() {
        let id = SessionId::new("session-456");
        assert_eq!(format!("{id}"), "session-456");
    }

    // -- BehaviorConfig defaults --

    #[test]
    fn behavior_config_default() {
        let b = BehaviorConfig::default();
        assert_eq!(b.max_iterations, 50);
        assert_eq!(b.max_retries, 3);
    }
}
