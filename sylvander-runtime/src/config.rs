//! Versioned, declarative server configuration.
//!
//! Configuration contains references to secrets, never inline credentials.
//! [`ServerConfig::validate`] resolves all cross-object identities before the
//! runtime starts any Agent, executor, or channel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sylvander_agent::spec::AgentSpec;

use sylvander_agent::prompt::{
    MAX_PROMPT_PROFILES, MAX_PROMPT_SELECTORS_PER_KIND, validate_identity, validate_profile_count,
    validate_profile_selectors, validate_prompt, validate_unique_identities,
};

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
    #[serde(default)]
    pub mode: ServerMode,
    #[serde(default = "default_server_name")]
    pub name: String,
    pub data_dir: Option<PathBuf>,
    pub session_db: Option<PathBuf>,
    pub memory_db: Option<PathBuf>,
    pub user_profile_db: Option<PathBuf>,
    pub workspace_journal: Option<PathBuf>,
    #[serde(default)]
    pub memory_maintenance: MemoryMaintenanceSettings,
    #[serde(default)]
    pub approval: ApprovalSettings,
    #[serde(default)]
    pub evidence: EvidenceSettings,
    #[serde(default)]
    pub boundary: BoundarySettings,
    #[serde(default)]
    pub identity: IdentitySettings,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            mode: ServerMode::default(),
            name: default_server_name(),
            data_dir: None,
            session_db: None,
            memory_db: None,
            user_profile_db: None,
            workspace_journal: None,
            memory_maintenance: MemoryMaintenanceSettings::default(),
            approval: ApprovalSettings::default(),
            evidence: EvidenceSettings::default(),
            boundary: BoundarySettings::default(),
            identity: IdentitySettings::default(),
        }
    }
}

/// Runtime trust profile. Production requires an independent memory integrity
/// anchor; self-use keeps durable `SQLite` memory without that external anchor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerMode {
    Production,
    #[default]
    SelfUse,
}

/// Optional durable stable-user identity service.
///
/// Supplying a digest key enables the service. Trusted issuers are exact
/// authenticated ingress identities allowed to request link challenges for a
/// configured stable user; request payloads can never select that user.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdentitySettings {
    pub database: Option<PathBuf>,
    pub digest_key: Option<SecretRef>,
    #[serde(default = "default_identity_challenge_ttl_seconds")]
    pub challenge_ttl_seconds: u32,
    #[serde(default)]
    pub trusted_issuers: Vec<IdentityIssuerSettings>,
}

impl Default for IdentitySettings {
    fn default() -> Self {
        Self {
            database: None,
            digest_key: None,
            challenge_ttl_seconds: default_identity_challenge_ttl_seconds(),
            trusted_issuers: Vec::new(),
        }
    }
}

impl std::fmt::Debug for IdentitySettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IdentitySettings")
            .field("database", &self.database)
            .field(
                "digest_key",
                &self.digest_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("challenge_ttl_seconds", &self.challenge_ttl_seconds)
            .field("trusted_issuer_count", &self.trusted_issuers.len())
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IdentityIssuerSettings {
    pub transport: String,
    pub channel_instance_id: String,
    pub principal_id: String,
    pub user_id: String,
}

const fn default_identity_challenge_ttl_seconds() -> u32 {
    300
}

/// Bounded production maintenance policy for durable Agent memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryMaintenanceSettings {
    #[serde(default)]
    pub integrity: MemoryIntegritySettings,
    #[serde(default)]
    pub retention: MemoryRetentionSettings,
    #[serde(default)]
    pub backup: MemoryBackupSettings,
    #[serde(default = "default_memory_maintenance_interval_seconds")]
    pub interval_seconds: u32,
    #[serde(default = "default_memory_maintenance_batch_size")]
    pub batch_size: u32,
    #[serde(default = "default_memory_maintenance_max_batches")]
    pub max_batches_per_run: u32,
}

impl Default for MemoryMaintenanceSettings {
    fn default() -> Self {
        Self {
            integrity: MemoryIntegritySettings::default(),
            retention: MemoryRetentionSettings::default(),
            backup: MemoryBackupSettings::default(),
            interval_seconds: default_memory_maintenance_interval_seconds(),
            batch_size: default_memory_maintenance_batch_size(),
            max_batches_per_run: default_memory_maintenance_max_batches(),
        }
    }
}

/// Independent authenticated trust anchor for relationship memory. Runtime
/// resolves every secret reference; raw secret bytes are never serialized.
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryIntegritySettings {
    pub key: Option<SecretRef>,
    pub backend: Option<MemoryIntegrityBackend>,
}

impl std::fmt::Debug for MemoryIntegritySettings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MemoryIntegritySettings")
            .field("key", &self.key.as_ref().map(|_| "[REDACTED]"))
            .field("backend", &self.backend)
            .finish()
    }
}

/// Latest-only integrity backend selection. The file backend protects against
/// a restricted database writer. The HTTP backend delegates monotonic compare
/// and swap to a separately administered service.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryIntegrityBackend {
    File {
        anchor_path: PathBuf,
    },
    Http {
        endpoint: String,
        bearer_token: SecretRef,
        #[serde(default)]
        ca_certificate: Option<SecretRef>,
        #[serde(default)]
        client_identity: Option<SecretRef>,
        #[serde(default = "default_memory_integrity_http_timeout_millis")]
        timeout_millis: u32,
        #[serde(default = "default_memory_integrity_http_read_retries")]
        read_retries: u8,
    },
}

impl std::fmt::Debug for MemoryIntegrityBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File { anchor_path } => formatter
                .debug_struct("File")
                .field("anchor_path", anchor_path)
                .finish(),
            Self::Http {
                timeout_millis,
                read_retries,
                ca_certificate,
                client_identity,
                ..
            } => formatter
                .debug_struct("Http")
                .field("endpoint", &"[REDACTED]")
                .field("bearer_token", &"[REDACTED]")
                .field(
                    "ca_certificate",
                    &ca_certificate.as_ref().map(|_| "[REDACTED]"),
                )
                .field(
                    "client_identity",
                    &client_identity.as_ref().map(|_| "[REDACTED]"),
                )
                .field("timeout_millis", timeout_millis)
                .field("read_retries", read_retries)
                .finish(),
        }
    }
}

impl MemoryIntegrityBackend {
    fn validate(&self, errors: &mut Vec<String>) {
        match self {
            Self::File { anchor_path } => {
                if !anchor_path.is_absolute() {
                    errors.push("server memory integrity file anchor_path must be absolute".into());
                }
            }
            Self::Http {
                endpoint,
                bearer_token,
                ca_certificate,
                client_identity,
                timeout_millis,
                read_retries,
            } => {
                let valid_endpoint = endpoint.len() <= 2_048
                    && url::Url::parse(endpoint).is_ok_and(|url| {
                        url.scheme() == "https"
                            && url.host_str().is_some()
                            && url.username().is_empty()
                            && url.password().is_none()
                            && url.query().is_none()
                            && url.fragment().is_none()
                    });
                if !valid_endpoint {
                    errors.push(
                        "server memory integrity HTTP endpoint must be an HTTPS URL without credentials, query, or fragment"
                            .into(),
                    );
                }
                bearer_token.validate("server memory integrity HTTP bearer_token", errors);
                if let Some(reference) = ca_certificate {
                    reference.validate("server memory integrity HTTP ca_certificate", errors);
                }
                if let Some(reference) = client_identity {
                    reference.validate("server memory integrity HTTP client_identity", errors);
                }
                if !(100..=30_000).contains(timeout_millis) {
                    errors.push(
                        "server memory integrity HTTP timeout_millis must be between 100 and 30000"
                            .into(),
                    );
                }
                if *read_retries > 3 {
                    errors
                        .push("server memory integrity HTTP read_retries must not exceed 3".into());
                }
            }
        }
    }
}

const fn default_memory_integrity_http_timeout_millis() -> u32 {
    5_000
}

const fn default_memory_integrity_http_read_retries() -> u8 {
    3
}

/// Finite backup schedule. Backup paths are derived from `data_dir` so a
/// configuration cannot redirect memory snapshots to an arbitrary location.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryBackupSettings {
    #[serde(default = "default_memory_backup_interval_seconds")]
    pub interval_seconds: u32,
    #[serde(default = "default_memory_backup_retained_copies")]
    pub retained_copies: u32,
}

impl Default for MemoryBackupSettings {
    fn default() -> Self {
        Self {
            interval_seconds: default_memory_backup_interval_seconds(),
            retained_copies: default_memory_backup_retained_copies(),
        }
    }
}

/// Finite lifetime and purge grace policy. No field permits unbounded storage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MemoryRetentionSettings {
    #[serde(default = "default_memory_retention_policy_revision")]
    pub revision: u64,
    #[serde(default = "default_memory_ttl_days")]
    pub default_ttl_days: u32,
    #[serde(default = "default_memory_max_ttl_days")]
    pub max_ttl_days: u32,
    #[serde(default = "default_expired_memory_grace_days")]
    pub expired_grace_days: u32,
    #[serde(default = "default_superseded_memory_retention_days")]
    pub superseded_retention_days: u32,
}

impl Default for MemoryRetentionSettings {
    fn default() -> Self {
        Self {
            revision: default_memory_retention_policy_revision(),
            default_ttl_days: default_memory_ttl_days(),
            max_ttl_days: default_memory_max_ttl_days(),
            expired_grace_days: default_expired_memory_grace_days(),
            superseded_retention_days: default_superseded_memory_retention_days(),
        }
    }
}

const fn default_memory_retention_policy_revision() -> u64 {
    1
}

const fn default_memory_ttl_days() -> u32 {
    365
}

const fn default_memory_max_ttl_days() -> u32 {
    5 * 365
}

const fn default_expired_memory_grace_days() -> u32 {
    7
}

const fn default_superseded_memory_retention_days() -> u32 {
    30
}

const fn default_memory_maintenance_interval_seconds() -> u32 {
    3_600
}

const fn default_memory_maintenance_batch_size() -> u32 {
    500
}

const fn default_memory_maintenance_max_batches() -> u32 {
    20
}

const fn default_memory_backup_interval_seconds() -> u32 {
    86_400
}

const fn default_memory_backup_retained_copies() -> u32 {
    7
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
    pub qualified_models: Vec<sylvander_protocol::ModelSelection>,
    /// Legacy singleton selectors retained for schema-v1 compatibility.
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
        let memory = &self.server.memory_maintenance;
        match (
            self.server.mode,
            memory.integrity.key.is_some(),
            memory.integrity.backend.is_some(),
        ) {
            (ServerMode::Production, false, false) => {
                errors.push("production mode requires a memory integrity key and backend".into());
            }
            (_, key, backend) if key != backend => {
                errors.push("memory integrity key and backend must be configured together".into());
            }
            _ => {}
        }
        if let Some(reference) = &memory.integrity.key {
            reference.validate("server memory integrity key", &mut errors);
        }
        if let Some(backend) = &memory.integrity.backend {
            backend.validate(&mut errors);
        }
        let retention = &memory.retention;
        if retention.revision == 0 || i64::try_from(retention.revision).is_err() {
            errors.push(
                "server memory_maintenance retention revision must be between 1 and 9223372036854775807"
                    .into(),
            );
        }
        if !(1..=5 * 365).contains(&retention.default_ttl_days) {
            errors.push(
                "server memory_maintenance retention default_ttl_days must be between 1 and 1825"
                    .into(),
            );
        }
        if !(1..=5 * 365).contains(&retention.max_ttl_days) {
            errors.push(
                "server memory_maintenance retention max_ttl_days must be between 1 and 1825"
                    .into(),
            );
        }
        if retention.default_ttl_days > retention.max_ttl_days {
            errors.push(
                "server memory_maintenance retention default_ttl_days must not exceed max_ttl_days"
                    .into(),
            );
        }
        if retention.expired_grace_days > 365 {
            errors.push(
                "server memory_maintenance retention expired_grace_days must not exceed 365".into(),
            );
        }
        if !(1..=3650).contains(&retention.superseded_retention_days) {
            errors.push(
                "server memory_maintenance retention superseded_retention_days must be between 1 and 3650"
                    .into(),
            );
        }
        if !(60..=86_400).contains(&memory.interval_seconds) {
            errors.push(
                "server memory_maintenance interval_seconds must be between 60 and 86400".into(),
            );
        }
        if !(1..=1_000).contains(&memory.batch_size) {
            errors.push("server memory_maintenance batch_size must be between 1 and 1000".into());
        }
        if !(1..=100).contains(&memory.max_batches_per_run) {
            errors.push(
                "server memory_maintenance max_batches_per_run must be between 1 and 100".into(),
            );
        }
        if !(3_600..=604_800).contains(&memory.backup.interval_seconds) {
            errors.push(
                "server memory_maintenance backup interval_seconds must be between 3600 and 604800"
                    .into(),
            );
        }
        if !(2..=30).contains(&memory.backup.retained_copies) {
            errors.push(
                "server memory_maintenance backup retained_copies must be between 2 and 30".into(),
            );
        }
        if !(1024..=16 * 1024 * 1024).contains(&self.server.boundary.max_request_bytes) {
            errors
                .push("server boundary max_request_bytes must be between 1024 and 16777216".into());
        }
        if !(1..=100_000).contains(&self.server.boundary.requests_per_minute) {
            errors.push("server boundary requests_per_minute must be between 1 and 100000".into());
        }
        validate_identity_settings(&self.server.identity, &mut errors);

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
            validate_agent_shape_and_environment(agent, &target_ids, &mut errors);
            validate_agent_model_catalog(agent, &provider_models, &mut errors);
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

    /// Validate one Agent without requiring its qualified Models to be in the
    /// boot-time provider catalog. Runtime administration uses this boundary
    /// because provider discovery may make additional Models available later.
    pub(crate) fn validate_agent_shape_and_environment(
        &self,
        agent: &AgentDefinitionConfig,
    ) -> Result<(), ConfigError> {
        let targets = self
            .execution_targets
            .iter()
            .map(|target| target.id.trim().to_string())
            .collect();
        let mut errors = Vec::new();
        validate_agent_shape_and_environment(agent, &targets, &mut errors);
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

fn validate_identity_settings(settings: &IdentitySettings, errors: &mut Vec<String>) {
    if let Some(reference) = &settings.digest_key {
        reference.validate("server identity digest_key", errors);
    }
    if !(30..=900).contains(&settings.challenge_ttl_seconds) {
        errors.push("server identity challenge_ttl_seconds must be between 30 and 900".into());
    }
    if settings.digest_key.is_none() && !settings.trusted_issuers.is_empty() {
        errors.push("server identity trusted_issuers require a digest_key".into());
    }
    if settings.digest_key.is_none() && settings.database.is_some() {
        errors.push("server identity database requires a digest_key".into());
    }
    if settings.digest_key.is_some() && settings.trusted_issuers.is_empty() {
        errors.push("server identity digest_key requires at least one trusted issuer".into());
    }
    let mut issuers = HashSet::new();
    for issuer in &settings.trusted_issuers {
        for (field, value) in [
            ("transport", issuer.transport.as_str()),
            ("channel_instance_id", issuer.channel_instance_id.as_str()),
            ("principal_id", issuer.principal_id.as_str()),
            ("user_id", issuer.user_id.as_str()),
        ] {
            if value.is_empty()
                || value.trim() != value
                || value.len() > 512
                || value.chars().any(char::is_control)
            {
                errors.push(format!("server identity issuer {field} is invalid"));
            }
        }
        if issuer.user_id == "__system__" {
            errors.push("server identity issuer user_id cannot be the system sentinel".into());
        }
        if !issuers.insert((
            issuer.transport.as_str(),
            issuer.channel_instance_id.as_str(),
            issuer.principal_id.as_str(),
        )) {
            errors.push("server identity has a duplicate trusted issuer ingress".into());
        }
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
        ExecutionTransportConfig::Local { root } => {
            if let Some(root) = root
                && (!root.is_absolute()
                    || root
                        .components()
                        .any(|component| component == std::path::Component::ParentDir))
            {
                errors.push(format!(
                    "local execution target {} root must be an absolute normalized path",
                    target.id
                ));
            }
        }
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

fn validate_agent_shape_and_environment(
    agent: &AgentDefinitionConfig,
    targets: &HashSet<String>,
    errors: &mut Vec<String>,
) {
    if agent.revision == 0 {
        errors.push(format!("Agent {} revision must be positive", agent.spec.id));
    }
    require_text("Agent name", &agent.spec.name, errors);
    require_text("Agent default provider", &agent.spec.model.provider, errors);
    require_text("Agent default model", &agent.spec.model.model_name, errors);
    validate_agent_prompts(agent, errors);
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
    if let Some(default) = &agent.default_prompt_profile
        && let Some(profile) = agent
            .prompt_profiles
            .iter()
            .find(|profile| profile.id.trim() == default.trim())
        && !prompt_profile_matches(
            profile,
            &agent.spec.model.provider,
            &agent.spec.model.model_name,
        )
    {
        errors.push(format!(
            "Agent {} default prompt profile {default} is incompatible with its default Model",
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

fn prompt_profile_matches(profile: &PromptProfileConfig, provider: &str, model: &str) -> bool {
    if !profile.qualified_models.is_empty() {
        return profile
            .qualified_models
            .iter()
            .any(|selection| selection.provider_id == provider && selection.model_id == model);
    }
    profile.providers.is_empty()
        || (profile
            .providers
            .first()
            .is_some_and(|value| value == provider)
            && profile.models.first().is_some_and(|value| value == model))
}

fn validate_agent_prompts(agent: &AgentDefinitionConfig, errors: &mut Vec<String>) {
    let result = validate_profile_count(agent.prompt_profiles.len())
        .and_then(|()| validate_prompt(&agent.spec.persona.system_prompt))
        .and_then(|()| {
            validate_unique_identities(
                agent
                    .prompt_profiles
                    .iter()
                    .map(|profile| profile.id.as_str()),
                MAX_PROMPT_PROFILES,
            )
        })
        .and_then(|()| match agent.default_prompt_profile.as_deref() {
            Some(default) => validate_identity(default),
            None => Ok(()),
        })
        .and_then(|()| {
            for profile in &agent.prompt_profiles {
                validate_prompt(&profile.system_prompt)?;
                validate_profile_selectors(
                    &profile.qualified_models,
                    &profile.providers,
                    &profile.models,
                )?;
                validate_unique_identities(
                    profile.providers.iter().map(String::as_str),
                    MAX_PROMPT_SELECTORS_PER_KIND,
                )?;
                validate_unique_identities(
                    profile.models.iter().map(String::as_str),
                    MAX_PROMPT_SELECTORS_PER_KIND,
                )?;
            }
            Ok(())
        });
    if let Err(issue) = result {
        errors.push(format!(
            "Agent {} prompt configuration is invalid: {issue}",
            agent.spec.id
        ));
    }
}

fn validate_agent_model_catalog(
    agent: &AgentDefinitionConfig,
    providers: &HashMap<String, HashSet<String>>,
    errors: &mut Vec<String>,
) {
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
    if !agent.spec.model.allowed_models.is_empty() {
        let default_model = agent.spec.model.model_name.trim();
        let mut allowed = HashSet::new();
        for model in &agent.spec.model.allowed_models {
            let allowed_provider = model.provider_id.trim();
            let allowed_model = model.model_id.trim();
            if allowed_provider.is_empty() || allowed_model.is_empty() {
                errors.push(format!(
                    "Agent {} allowed Model identities must not be empty",
                    agent.spec.id
                ));
                continue;
            }
            if !allowed.insert((allowed_provider, allowed_model)) {
                errors.push(format!(
                    "Agent {} has duplicate allowed Model {}/{}",
                    agent.spec.id, model.provider_id, model.model_id
                ));
            }
            match providers.get(allowed_provider) {
                None => errors.push(format!(
                    "Agent {} allowed Model {}/{} references unknown provider {}",
                    agent.spec.id, model.provider_id, model.model_id, model.provider_id
                )),
                Some(models) if !models.contains(allowed_model) => errors.push(format!(
                    "Agent {} allowed Model {} is absent from provider {}",
                    agent.spec.id, model.model_id, model.provider_id
                )),
                Some(_) => {}
            }
        }
        if !allowed.contains(&(provider, default_model)) {
            errors.push(format!(
                "Agent {} allowed Models do not contain its default {}/{}",
                agent.spec.id, agent.spec.model.provider, agent.spec.model.model_name
            ));
        }
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

#[cfg(test)]
mod qualified_model_tests {
    use super::*;

    fn qualified_config() -> ServerConfig {
        ServerConfig::from_toml(
            r#"
schema_version = 1

[server]
mode = "self_use"

[[model_providers]]
id = "alpha"
base_url = "https://alpha.example.invalid"
[model_providers.api_key]
source = "env"
name = "ALPHA_TOKEN"
[[model_providers.models]]
id = "shared"

[[model_providers]]
id = "beta"
base_url = "https://beta.example.invalid"
[model_providers.api_key]
source = "env"
name = "BETA_TOKEN"
[[model_providers.models]]
id = "shared"

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
allowed_models = [
  { provider_id = "alpha", model_id = "shared" },
  { provider_id = "beta", model_id = "shared" },
]
"#,
        )
        .expect("qualified configuration")
    }

    fn validation_text(config: &ServerConfig) -> String {
        config.validate().unwrap_err().errors.join("\n")
    }

    #[test]
    fn qualified_allowlist_accepts_same_model_id_across_providers() {
        qualified_config().validate().unwrap();
    }

    #[test]
    fn qualified_allowlist_rejects_unknown_provider() {
        let mut config = qualified_config();
        config.agents[0].spec.model.allowed_models[1].provider_id = "missing".into();
        assert!(validation_text(&config).contains("references unknown provider missing"));
    }

    #[test]
    fn qualified_allowlist_rejects_missing_model() {
        let mut config = qualified_config();
        config.agents[0].spec.model.allowed_models[1].model_id = "missing".into();
        assert!(validation_text(&config).contains("absent from provider beta"));
    }

    #[test]
    fn qualified_allowlist_rejects_duplicate_exact_pair() {
        let mut config = qualified_config();
        let duplicate = config.agents[0].spec.model.allowed_models[0].clone();
        config.agents[0].spec.model.allowed_models[1] = duplicate;
        assert!(validation_text(&config).contains("duplicate allowed Model alpha/shared"));
    }

    #[test]
    fn qualified_allowlist_rejects_missing_default_pair() {
        let mut config = qualified_config();
        config.agents[0].spec.model.allowed_models.remove(0);
        assert!(validation_text(&config).contains("do not contain its default alpha/shared"));
    }
}
