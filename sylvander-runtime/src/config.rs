//! Versioned, declarative server configuration.
//!
//! Configuration contains references to secrets, never inline credentials.
//! [`ServerConfig::validate`] resolves all cross-object identities before the
//! runtime starts any Agent, executor, or channel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sylvander_agent::spec::AgentSpec;

mod legacy;
mod secret;

pub use secret::{SecretResolver, SecretValue, SystemSecretResolver};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub server: ServerSettings,
    #[serde(default)]
    pub model_providers: Vec<ModelProviderConfig>,
    #[serde(default)]
    pub execution_targets: Vec<ExecutionTargetConfig>,
    #[serde(default)]
    pub agents: Vec<AgentDefinitionConfig>,
    #[serde(default)]
    pub channels: Vec<ChannelInstanceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSettings {
    #[serde(default = "default_server_name")]
    pub name: String,
    pub data_dir: Option<PathBuf>,
    pub session_db: Option<PathBuf>,
    pub workspace_journal: Option<PathBuf>,
    #[serde(default)]
    pub approval: ApprovalSettings,
    #[serde(default)]
    pub evidence: EvidenceSettings,
    #[serde(default)]
    pub boundary: BoundarySettings,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            name: default_server_name(),
            data_dir: None,
            session_db: None,
            workspace_journal: None,
            approval: ApprovalSettings::default(),
            evidence: EvidenceSettings::default(),
            boundary: BoundarySettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundarySettings {
    #[serde(default = "default_max_request_bytes")]
    pub max_request_bytes: usize,
    #[serde(default = "default_requests_per_minute")]
    pub requests_per_minute: u32,
}

impl Default for BoundarySettings {
    fn default() -> Self {
        Self {
            max_request_bytes: default_max_request_bytes(),
            requests_per_minute: default_requests_per_minute(),
        }
    }
}

const fn default_max_request_bytes() -> usize {
    1024 * 1024
}

const fn default_requests_per_minute() -> u32 {
    240
}

/// Durable runtime evidence used for recovery, audit, evaluation, and
/// human-gated improvement proposals.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSettings {
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub path: Option<PathBuf>,
    #[serde(default = "default_evidence_retention_days")]
    pub retention_days: u32,
    #[serde(default)]
    pub content: EvidenceContentPolicy,
}

impl Default for EvidenceSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            retention_days: default_evidence_retention_days(),
            content: EvidenceContentPolicy::MetadataOnly,
        }
    }
}

const fn default_evidence_retention_days() -> u32 {
    30
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceContentPolicy {
    /// Store event types, sizes, timings, and digests, but no raw content.
    #[default]
    MetadataOnly,
    /// Store structurally redacted payloads where a recorder supports them.
    Redacted,
    /// Store complete payloads. This must be an explicit operator decision.
    Full,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalSettings {
    #[serde(default)]
    pub enabled: bool,
    pub persistent_store: Option<PathBuf>,
}

fn default_server_name() -> String {
    "sylvander".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum SecretRef {
    Env { name: String },
    File { path: PathBuf },
}

impl SecretRef {
    fn validate(&self, field: &str, errors: &mut Vec<String>) {
        match self {
            Self::Env { name } if name.trim().is_empty() => {
                errors.push(format!("{field} environment variable name is empty"));
            }
            Self::File { path } if path.as_os_str().is_empty() => {
                errors.push(format!("{field} secret file path is empty"));
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProviderConfig {
    pub id: String,
    #[serde(default = "default_anthropic_kind")]
    pub kind: String,
    pub base_url: String,
    pub api_key: SecretRef,
    #[serde(default)]
    pub models: Vec<ModelDefinitionConfig>,
}

fn default_anthropic_kind() -> String {
    "anthropic_compatible".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDefinitionConfig {
    pub id: String,
    #[serde(default = "default_context_window")]
    pub context_window: u32,
    #[serde(default = "default_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

const fn default_context_window() -> u32 {
    200_000
}

const fn default_output_tokens() -> u32 {
    32_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionTargetConfig {
    pub id: String,
    pub transport: ExecutionTransportConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExecutionTransportConfig {
    Local {
        root: Option<PathBuf>,
    },
    Ssh {
        host: String,
        #[serde(default = "default_ssh_port")]
        port: u16,
        user: String,
        credential: SecretRef,
    },
    Container {
        runtime: String,
        image: String,
    },
    Sandbox {
        driver: String,
        profile: String,
    },
}

const fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceBindingConfig {
    pub execution_target: String,
    pub path: String,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptProfileConfig {
    pub id: String,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    pub system_prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinitionConfig {
    #[serde(default = "default_revision")]
    pub revision: u64,
    pub spec: AgentSpec,
    pub agent_workspace: Option<WorkspaceBindingConfig>,
    #[serde(default)]
    pub prompt_profiles: Vec<PromptProfileConfig>,
    pub default_prompt_profile: Option<String>,
    #[serde(default)]
    pub allow_session_prompt: bool,
    #[serde(default)]
    pub access: AgentAccessConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAccessConfig {
    #[serde(default)]
    pub allow_authenticated: bool,
    #[serde(default)]
    pub allowed_principals: Vec<String>,
    #[serde(default)]
    pub allowed_roles: Vec<String>,
}

const fn default_revision() -> u64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelInstanceConfig {
    pub id: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub default_agent: String,
    pub default_workspace: Option<WorkspaceBindingConfig>,
    pub transport: ChannelTransportConfig,
}

const fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChannelTransportConfig {
    Unix {
        path: PathBuf,
    },
    Http {
        bind: String,
        principal_id: String,
        bearer_token: SecretRef,
    },
    Websocket {
        bind: String,
        principal_id: String,
        bearer_token: SecretRef,
    },
    DingTalk {
        app_key: SecretRef,
        app_secret: SecretRef,
    },
    Telegram {
        token: SecretRef,
        bind: String,
        webhook_secret: SecretRef,
    },
    Wechat {
        bind: String,
        corp_id: String,
        agent_id: String,
        secret: SecretRef,
        token: SecretRef,
        encoding_aes_key: SecretRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid Sylvander configuration:\n{}", .errors.join("\n"))]
pub struct ConfigError {
    pub errors: Vec<String>,
}

impl ServerConfig {
    /// Parse TOML and reject oversized or invalid configurations.
    pub fn from_toml(input: &str) -> Result<Self, ConfigError> {
        const MAX_CONFIG_BYTES: usize = 1024 * 1024;
        if input.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError {
                errors: vec![format!(
                    "configuration exceeds {MAX_CONFIG_BYTES} byte limit"
                )],
            });
        }
        let config = toml::from_str::<Self>(input).map_err(|error| ConfigError {
            errors: vec![format!("TOML parse failed: {error}")],
        })?;
        config.validate()?;
        Ok(config)
    }

    /// Read and validate a TOML configuration file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let input = std::fs::read_to_string(path).map_err(|error| ConfigError {
            errors: vec![format!("failed to read {}: {error}", path.display())],
        })?;
        Self::from_toml(&input)
    }

    /// Validate identities, references, and safety-sensitive values.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut errors = Vec::new();
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            errors.push(format!(
                "unsupported schema_version {}; expected {CONFIG_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        require_text("server.name", &self.server.name, &mut errors);
        if self.server.evidence.enabled
            && !(1..=3650).contains(&self.server.evidence.retention_days)
        {
            errors.push("server evidence retention_days must be between 1 and 3650".into());
        }
        if !(1024..=16 * 1024 * 1024).contains(&self.server.boundary.max_request_bytes) {
            errors
                .push("server boundary max_request_bytes must be between 1024 and 16777216".into());
        }
        if !(1..=100_000).contains(&self.server.boundary.requests_per_minute) {
            errors.push("server boundary requests_per_minute must be between 1 and 100000".into());
        }

        unique_ids(
            "model provider",
            self.model_providers.iter().map(|item| item.id.as_str()),
            &mut errors,
        );
        let mut provider_models = HashMap::new();
        for provider in &self.model_providers {
            require_text("model provider kind", &provider.kind, &mut errors);
            if provider.kind != "anthropic_compatible" {
                errors.push(format!(
                    "model provider {} has unsupported kind {}",
                    provider.id, provider.kind
                ));
            }
            require_text("model provider base_url", &provider.base_url, &mut errors);
            provider
                .api_key
                .validate("model provider api_key", &mut errors);
            let models = unique_ids(
                &format!("model in provider {}", provider.id),
                provider.models.iter().map(|model| model.id.as_str()),
                &mut errors,
            );
            provider_models.insert(provider.id.trim().to_string(), models);
            for model in &provider.models {
                if model.context_window == 0 || model.max_output_tokens == 0 {
                    errors.push(format!(
                        "model {}/{} token limits must be positive",
                        provider.id, model.id
                    ));
                }
            }
        }

        let target_ids = unique_ids(
            "execution target",
            self.execution_targets.iter().map(|item| item.id.as_str()),
            &mut errors,
        );
        for target in &self.execution_targets {
            validate_execution_target(target, &mut errors);
        }

        let agent_ids = unique_ids(
            "Agent",
            self.agents.iter().map(|item| item.spec.id.0.as_str()),
            &mut errors,
        );
        for agent in &self.agents {
            validate_agent(agent, &provider_models, &target_ids, &mut errors);
        }

        unique_ids(
            "channel instance",
            self.channels.iter().map(|item| item.id.as_str()),
            &mut errors,
        );
        for channel in &self.channels {
            if !agent_ids.contains(channel.default_agent.trim()) {
                errors.push(format!(
                    "channel {} references unknown Agent {}",
                    channel.id, channel.default_agent
                ));
            }
            if let Some(workspace) = &channel.default_workspace {
                validate_workspace(workspace, &target_ids, "channel workspace", &mut errors);
            }
            validate_channel(channel, &mut errors);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(ConfigError { errors })
        }
    }
}

fn require_text(field: &str, value: &str, errors: &mut Vec<String>) {
    if value.trim().is_empty() {
        errors.push(format!("{field} is empty"));
    }
}

fn unique_ids<'a>(
    kind: &str,
    values: impl Iterator<Item = &'a str>,
    errors: &mut Vec<String>,
) -> HashSet<String> {
    let mut seen = HashSet::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            errors.push(format!("{kind} id is empty"));
        } else if !seen.insert(value.to_string()) {
            errors.push(format!("duplicate {kind} id `{value}`"));
        }
    }
    seen
}

fn validate_workspace(
    workspace: &WorkspaceBindingConfig,
    targets: &HashSet<String>,
    field: &str,
    errors: &mut Vec<String>,
) {
    if !targets.contains(workspace.execution_target.trim()) {
        errors.push(format!(
            "{field} references unknown execution target {}",
            workspace.execution_target
        ));
    }
    require_text(&format!("{field} path"), &workspace.path, errors);
}

fn validate_execution_target(target: &ExecutionTargetConfig, errors: &mut Vec<String>) {
    match &target.transport {
        ExecutionTransportConfig::Local { .. } => {}
        ExecutionTransportConfig::Ssh {
            host,
            port,
            user,
            credential,
        } => {
            require_text("SSH host", host, errors);
            require_text("SSH user", user, errors);
            if *port == 0 {
                errors.push(format!("SSH target {} port must be positive", target.id));
            }
            credential.validate("SSH credential", errors);
        }
        ExecutionTransportConfig::Container { runtime, image } => {
            require_text("container runtime", runtime, errors);
            require_text("container image", image, errors);
        }
        ExecutionTransportConfig::Sandbox { driver, profile } => {
            require_text("sandbox driver", driver, errors);
            require_text("sandbox profile", profile, errors);
        }
    }
}

fn validate_agent(
    agent: &AgentDefinitionConfig,
    providers: &HashMap<String, HashSet<String>>,
    targets: &HashSet<String>,
    errors: &mut Vec<String>,
) {
    if agent.revision == 0 {
        errors.push(format!("Agent {} revision must be positive", agent.spec.id));
    }
    require_text("Agent name", &agent.spec.name, errors);
    require_text("Agent default model", &agent.spec.model.model_name, errors);
    let provider = agent.spec.model.provider.trim();
    if !providers.contains_key(provider) {
        errors.push(format!(
            "Agent {} references unknown model provider {}",
            agent.spec.id, agent.spec.model.provider
        ));
    } else if !providers[provider].contains(agent.spec.model.model_name.trim()) {
        errors.push(format!(
            "Agent {} references model {} absent from provider {}",
            agent.spec.id, agent.spec.model.model_name, agent.spec.model.provider
        ));
    }
    if let Some(workspace) = &agent.agent_workspace {
        validate_workspace(workspace, targets, "Agent workspace", errors);
    }
    let profiles = unique_ids(
        &format!("prompt profile for Agent {}", agent.spec.id),
        agent.prompt_profiles.iter().map(|item| item.id.as_str()),
        errors,
    );
    if let Some(default) = &agent.default_prompt_profile
        && !profiles.contains(default.trim())
    {
        errors.push(format!(
            "Agent {} references unknown prompt profile {default}",
            agent.spec.id
        ));
    }
    for principal in &agent.access.allowed_principals {
        require_text("Agent allowed principal", principal, errors);
    }
    for role in &agent.access.allowed_roles {
        require_text("Agent allowed role", role, errors);
    }
}

fn validate_channel(channel: &ChannelInstanceConfig, errors: &mut Vec<String>) {
    match &channel.transport {
        ChannelTransportConfig::Unix { path } => {
            if path.as_os_str().is_empty() {
                errors.push(format!("channel {} Unix path is empty", channel.id));
            }
        }
        ChannelTransportConfig::Http {
            bind,
            principal_id,
            bearer_token,
        } => {
            require_text("HTTP bind", bind, errors);
            require_text("HTTP principal_id", principal_id, errors);
            bearer_token.validate("HTTP bearer_token", errors);
        }
        ChannelTransportConfig::Websocket {
            bind,
            principal_id,
            bearer_token,
        } => {
            require_text("WebSocket bind", bind, errors);
            require_text("WebSocket principal_id", principal_id, errors);
            bearer_token.validate("WebSocket bearer_token", errors);
        }
        ChannelTransportConfig::DingTalk {
            app_key,
            app_secret,
        } => {
            app_key.validate("DingTalk app_key", errors);
            app_secret.validate("DingTalk app_secret", errors);
        }
        ChannelTransportConfig::Telegram {
            token,
            bind,
            webhook_secret,
        } => {
            token.validate("Telegram token", errors);
            webhook_secret.validate("Telegram webhook_secret", errors);
            require_text("Telegram bind", bind, errors);
        }
        ChannelTransportConfig::Wechat {
            bind,
            corp_id,
            agent_id,
            secret,
            token,
            encoding_aes_key,
        } => {
            require_text("WeChat bind", bind, errors);
            require_text("WeChat corp_id", corp_id, errors);
            require_text("WeChat agent_id", agent_id, errors);
            secret.validate("WeChat secret", errors);
            token.validate("WeChat token", errors);
            encoding_aes_key.validate("WeChat encoding_aes_key", errors);
        }
    }
}

#[cfg(test)]
mod tests;
