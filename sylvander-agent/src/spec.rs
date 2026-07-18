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

pub use sylvander_protocol::{AgentId, ModelSelection, SessionId};

// ---------------------------------------------------------------------------
// Config sub-types
// ---------------------------------------------------------------------------

/// Agent personality — the "soul" of an agent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersonaConfig {
    /// System prompt injected at the start of every conversation.
    #[serde(default)]
    pub system_prompt: String,
    /// Human-readable description of the agent's role.
    #[serde(default)]
    pub description: String,
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
    /// Exact Provider/Model identities this Agent revision may select.
    ///
    /// Runtime configuration requires this list to be explicit, non-empty,
    /// provider-qualified, and to contain the Agent default model.
    pub allowed_models: Vec<ModelSelection>,
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
            allowed_models: Vec::new(),
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

/// A workspace-owned prompt command advertised to interactive UI clients.
/// It expands through the ordinary chat path and cannot invoke presentation
/// callbacks or bypass the Agent's permission and approval boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiCommandConfig {
    /// Stable identity used for collision and duplicate detection.
    pub id: String,
    /// Slash command name without the leading slash.
    pub name: String,
    /// Human-readable usage, normally beginning with `/name`.
    pub usage: String,
    /// Short command-palette description.
    pub description: String,
    /// Optional compact provenance or behavior hint.
    #[serde(default)]
    pub hint: String,
    /// Prompt template submitted when invoked. `{{args}}` expands to the
    /// command-line arguments.
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPresentationConfig {
    pub tool_name: String,
    pub label: String,
    pub kind: sylvander_protocol::ToolPresentationKind,
    #[serde(default)]
    pub target_field: Option<String>,
}

impl MemoryStoreConfig {
    /// Resolve this config into an actual [`MemoryStore`](crate::tools::MemoryStore) implementation.
    ///
    /// Supports `"in_memory"` and `"sqlite"`.
    ///
    /// # Errors
    /// Returns an error for unknown store types.
    pub fn build(
        &self,
    ) -> Result<
        std::sync::Arc<dyn crate::tools::memory::MemoryStore>,
        crate::tools::memory::MemoryStoreError,
    > {
        match self.store_type.as_str() {
            "in_memory" => Ok(std::sync::Arc::new(
                crate::tools::memory::InMemoryMemoryStore::new(),
            )),
            "sqlite" => Ok(std::sync::Arc::new(
                crate::tools::memory_sqlite::SqliteMemoryStore::open(&self.path)?,
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
    /// Workspace-owned prompt commands exposed to interactive UIs.
    #[serde(default)]
    pub ui_commands: Vec<UiCommandConfig>,
    /// Before-tool hooks executed through the selected workspace executor.
    #[serde(default)]
    pub hooks: Vec<crate::tool::ToolHookConfig>,
    /// Declarative TUI presentation hints for extension-provided tools.
    #[serde(default)]
    pub tool_presentations: Vec<ToolPresentationConfig>,
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
    ui_commands: Vec<UiCommandConfig>,
    hooks: Vec<crate::tool::ToolHookConfig>,
    tool_presentations: Vec<ToolPresentationConfig>,
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

    /// Allow one exact Provider/Model identity for this Agent revision.
    #[must_use]
    pub fn allowed_model(
        mut self,
        provider_id: impl Into<String>,
        model_id: impl Into<String>,
    ) -> Self {
        self.model.allowed_models.push(ModelSelection {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        });
        self
    }

    // -- tools --

    /// Register a built-in tool by name.
    #[must_use]
    pub fn builtin_tool(mut self, name: impl Into<String>) -> Self {
        self.tools.push(ToolRef::Builtin { name: name.into() });
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

    /// Replace the before-tool hook set.
    #[must_use]
    pub fn hooks(mut self, hooks: Vec<crate::tool::ToolHookConfig>) -> Self {
        self.hooks = hooks;
        self
    }

    #[must_use]
    pub fn tool_presentations(mut self, presentations: Vec<ToolPresentationConfig>) -> Self {
        self.tool_presentations = presentations;
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

    /// Add a workspace-owned prompt command.
    #[must_use]
    pub fn ui_command(mut self, config: UiCommandConfig) -> Self {
        self.ui_commands.push(config);
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
            ui_commands: self.ui_commands,
            hooks: self.hooks,
            tool_presentations: self.tool_presentations,
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
#[path = "../tests/unit/spec.rs"]
mod tests;
