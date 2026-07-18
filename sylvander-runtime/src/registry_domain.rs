//! Internal persisted Provider, Model, and Credential registry domain.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use sylvander_protocol::{ModelLifecycle, ModelPricing};

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::config::SecretRef;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderDefinition {
    pub id: String,
    pub revision: u64,
    pub kind: String,
    pub base_url: String,
    pub credential_binding_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelDefinition {
    pub provider_id: String,
    pub model_id: String,
    pub revision: u64,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub capabilities: BTreeSet<String>,
    pub lifecycle: ModelLifecycle,
    pub pricing: Option<ModelPricing>,
}

/// Capability vocabulary persisted by the registry and consumed by adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum CanonicalModelCapability {
    ExtendedThinking,
    PromptCaching,
    StructuredOutput,
    ToolUse,
    Vision,
    DocumentInput,
}

impl CanonicalModelCapability {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ExtendedThinking => "extended_thinking",
            Self::PromptCaching => "prompt_caching",
            Self::StructuredOutput => "structured_output",
            Self::ToolUse => "tool_use",
            Self::Vision => "vision",
            Self::DocumentInput => "document_input",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "extended_thinking" | "reasoning" => Some(Self::ExtendedThinking),
            "prompt_caching" => Some(Self::PromptCaching),
            "structured_output" => Some(Self::StructuredOutput),
            "tool_use" => Some(Self::ToolUse),
            "vision" => Some(Self::Vision),
            "document_input" => Some(Self::DocumentInput),
            _ => None,
        }
    }
}

/// Parse capability input without silently repairing malformed identities.
pub(crate) fn parse_model_capabilities<I, S>(
    capabilities: I,
) -> Result<BTreeSet<CanonicalModelCapability>, ModelCapabilityError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut parsed = BTreeSet::new();
    for capability in capabilities {
        let raw = capability.as_ref();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(ModelCapabilityError::Blank);
        }
        if raw != trimmed {
            return Err(ModelCapabilityError::SurroundingWhitespace(raw.to_owned()));
        }
        let Some(capability) = CanonicalModelCapability::parse(trimmed) else {
            if CanonicalModelCapability::parse(&trimmed.to_ascii_lowercase()).is_some() {
                return Err(ModelCapabilityError::NotLowercase(raw.to_owned()));
            }
            return Err(ModelCapabilityError::Unknown(raw.to_owned()));
        };
        if !parsed.insert(capability) {
            return Err(ModelCapabilityError::Duplicate(capability));
        }
    }
    Ok(parsed)
}

/// Canonicalize ingress values for new persisted Model revisions.
///
/// The historical `reasoning` alias is accepted and emitted as
/// `extended_thinking`. Callers validating an existing revision may discard
/// this return value, preserving its original JSON and digest.
pub(crate) fn canonicalize_model_capabilities<I, S>(
    capabilities: I,
) -> Result<BTreeSet<String>, ModelCapabilityError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parse_model_capabilities(capabilities).map(|parsed| {
        parsed
            .into_iter()
            .map(|capability| capability.as_str().to_owned())
            .collect()
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ModelCapabilityError {
    #[error("model capability must not be blank")]
    Blank,
    #[error("model capability `{0}` has surrounding whitespace")]
    SurroundingWhitespace(String),
    #[error("model capability `{0}` must use lowercase canonical spelling")]
    NotLowercase(String),
    #[error("unknown model capability `{0}`")]
    Unknown(String),
    #[error("duplicate model capability `{0}`")]
    Duplicate(CanonicalModelCapability),
}

/// Content-free capability failure category safe to cross runtime boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ModelCapabilityIssue {
    #[error("blank capability")]
    Blank,
    #[error("capability has surrounding whitespace")]
    SurroundingWhitespace,
    #[error("capability uses non-canonical case")]
    NotLowercase,
    #[error("unknown capability")]
    Unknown,
    #[error("duplicate capability")]
    Duplicate,
}

impl ModelCapabilityError {
    #[must_use]
    pub(crate) const fn issue(&self) -> ModelCapabilityIssue {
        match self {
            Self::Blank => ModelCapabilityIssue::Blank,
            Self::SurroundingWhitespace(_) => ModelCapabilityIssue::SurroundingWhitespace,
            Self::NotLowercase(_) => ModelCapabilityIssue::NotLowercase,
            Self::Unknown(_) => ModelCapabilityIssue::Unknown,
            Self::Duplicate(_) => ModelCapabilityIssue::Duplicate,
        }
    }
}

impl std::fmt::Display for CanonicalModelCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CredentialBindingRevision {
    pub binding_id: String,
    pub generation: u64,
    pub reference: SecretRef,
}

impl std::fmt::Debug for CredentialBindingRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialBindingRevision")
            .field("binding_id", &self.binding_id)
            .field("generation", &self.generation)
            .field("reference", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredRevision<T> {
    pub definition: T,
    pub digest: String,
    pub created_at: i64,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SecretReferenceKind {
    Environment,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CredentialBindingView {
    pub binding_id: String,
    pub generation: u64,
    pub reference_kind: SecretReferenceKind,
    pub reference_configured: bool,
    pub reference_digest_sha256: String,
    pub created_at: i64,
    pub active: bool,
}

impl ProviderDefinition {
    pub(crate) fn validate(&self) -> Result<(), AgentRegistryError> {
        require(
            self.revision,
            &[
                &self.id,
                &self.kind,
                &self.base_url,
                &self.credential_binding_id,
            ],
        )
    }
}

impl ModelDefinition {
    pub(crate) fn validate(&self) -> Result<(), AgentRegistryError> {
        require(self.revision, &[&self.provider_id, &self.model_id])?;
        if self.context_window == 0 || self.max_output_tokens == 0 {
            return Err(AgentRegistryError::Invalid(
                "model token limits must be positive".into(),
            ));
        }
        parse_model_capabilities(&self.capabilities).map_err(|error| {
            AgentRegistryError::Invalid(format!(
                "invalid model capability metadata: {}",
                error.issue()
            ))
        })?;
        Ok(())
    }
}

impl CredentialBindingRevision {
    pub(crate) fn validate(&self) -> Result<(), AgentRegistryError> {
        require(self.generation, &[&self.binding_id])?;
        let present = match &self.reference {
            SecretRef::Env { name } => !name.trim().is_empty(),
            SecretRef::File { path } => !path.as_os_str().is_empty(),
        };
        present.then_some(()).ok_or_else(|| {
            AgentRegistryError::Invalid("credential reference must be configured".into())
        })
    }

    fn redacted(
        &self,
        stored: &StoredRevision<Self>,
    ) -> Result<CredentialBindingView, AgentRegistryError> {
        Ok(CredentialBindingView {
            binding_id: self.binding_id.clone(),
            generation: self.generation,
            reference_kind: match &self.reference {
                SecretRef::Env { .. } => SecretReferenceKind::Environment,
                SecretRef::File { .. } => SecretReferenceKind::File,
            },
            reference_configured: true,
            reference_digest_sha256: digest(&canonical_json(&self.reference)?),
            created_at: stored.created_at,
            active: stored.active,
        })
    }
}

fn require(version: u64, fields: &[&str]) -> Result<(), AgentRegistryError> {
    if version == 0 || fields.iter().any(|value| value.trim().is_empty()) {
        return Err(AgentRegistryError::Invalid(
            "registry identity and version must be set".into(),
        ));
    }
    Ok(())
}

pub(crate) trait DefinitionDocument: Serialize {}
impl DefinitionDocument for ProviderDefinition {}
impl DefinitionDocument for ModelDefinition {}

pub(crate) fn canonical_definition<T: DefinitionDocument>(
    value: &T,
) -> Result<(String, String), AgentRegistryError> {
    let json = canonical_json(value)?;
    let digest = digest(&json);
    Ok((json, digest))
}

pub(crate) fn canonical_secret_reference(
    reference: &SecretRef,
) -> Result<(String, String), AgentRegistryError> {
    let json = canonical_json(reference)?;
    let digest = digest(&json);
    Ok((json, digest))
}

fn canonical_json<T: Serialize>(value: &T) -> Result<String, AgentRegistryError> {
    serde_json::to_string(value).map_err(AgentRegistryError::serde)
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

impl AgentRegistry {
    pub(crate) async fn load_provider_revision(
        &self,
        provider_id: &str,
        revision: u64,
    ) -> Result<Option<StoredRevision<ProviderDefinition>>, AgentRegistryError> {
        load(
            self,
            "provider_definitions",
            "provider_registry_heads",
            &[provider_id],
            revision,
        )
        .await
    }

    pub(crate) async fn load_model_revision(
        &self,
        provider_id: &str,
        model_id: &str,
        revision: u64,
    ) -> Result<Option<StoredRevision<ModelDefinition>>, AgentRegistryError> {
        load(
            self,
            "model_definitions",
            "model_registry_heads",
            &[provider_id, model_id],
            revision,
        )
        .await
    }

    pub(crate) async fn load_credential_revision(
        &self,
        binding_id: &str,
        generation: u64,
    ) -> Result<Option<StoredRevision<CredentialBindingRevision>>, AgentRegistryError> {
        let binding_id = binding_id.to_owned();
        let generation = i64::try_from(generation).map_err(|_| {
            AgentRegistryError::Invalid("credential generation exceeds SQLite range".into())
        })?;
        self.run(move |connection| {
            let row = connection
                .query_row(
                    "SELECT d.reference_json,d.digest,d.created_at,\
                     COALESCE(h.active_generation=d.generation,0) \
                     FROM credential_binding_revisions d LEFT JOIN credential_binding_heads h \
                     ON h.binding_id=d.binding_id WHERE d.binding_id=?1 AND d.generation=?2",
                    params![binding_id, generation],
                    read_row,
                )
                .optional()
                .map_err(AgentRegistryError::sqlite)?;
            row.map(|(json, expected, created_at, active)| {
                if digest(&json) != expected {
                    return Err(AgentRegistryError::Integrity(
                        "credential reference digest mismatch".into(),
                    ));
                }
                let definition = CredentialBindingRevision {
                    binding_id,
                    generation: u64::try_from(generation).map_err(|_| {
                        AgentRegistryError::Integrity("negative credential generation".into())
                    })?,
                    reference: serde_json::from_str(&json).map_err(AgentRegistryError::serde)?,
                };
                definition.validate()?;
                Ok(StoredRevision {
                    definition,
                    digest: expected,
                    created_at,
                    active,
                })
            })
            .transpose()
        })
        .await
    }

    pub(crate) async fn inspect_credential_revision(
        &self,
        binding_id: &str,
        generation: u64,
    ) -> Result<Option<CredentialBindingView>, AgentRegistryError> {
        self.load_credential_revision(binding_id, generation)
            .await?
            .map(|stored| stored.definition.clone().redacted(&stored))
            .transpose()
    }
}

async fn load<T: DeserializeOwned + ValidateIdentity + Send + 'static>(
    registry: &AgentRegistry,
    definitions: &'static str,
    heads: &'static str,
    identity: &[&str],
    version: u64,
) -> Result<Option<StoredRevision<T>>, AgentRegistryError> {
    let identity = identity
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    let version = i64::try_from(version)
        .map_err(|_| AgentRegistryError::Invalid("registry version exceeds SQLite range".into()))?;
    registry.run(move |connection| {
        let sql = match identity.as_slice() {
            [_] => format!("SELECT d.definition_json,d.digest,d.created_at,COALESCE(h.active_{}=d.{},0) FROM {definitions} d LEFT JOIN {heads} h ON h.{}=d.{} WHERE d.{}=?1 AND d.{}=?2", T::VERSION, T::VERSION, T::ID1, T::ID1, T::ID1, T::VERSION),
            [_, _] => format!("SELECT d.definition_json,d.digest,d.created_at,COALESCE(h.active_revision=d.revision,0) FROM {definitions} d LEFT JOIN {heads} h ON h.provider_id=d.provider_id AND h.model_id=d.model_id WHERE d.provider_id=?1 AND d.model_id=?2 AND d.revision=?3"),
            _ => unreachable!(),
        };
        let row = match identity.as_slice() {
            [id] => connection.query_row(&sql, params![id, version], read_row).optional(),
            [provider, model] => connection.query_row(&sql, params![provider, model, version], read_row).optional(),
            _ => unreachable!(),
        }.map_err(AgentRegistryError::sqlite)?;
        row.map(|stored| decode_stored(stored, &identity, version)).transpose()
    }).await
}

trait ValidateIdentity {
    const ID1: &'static str;
    const VERSION: &'static str;
    fn validate_identity(
        &self,
        identity: &[String],
        version: u64,
    ) -> Result<(), AgentRegistryError>;
}

impl ValidateIdentity for ProviderDefinition {
    const ID1: &'static str = "provider_id";
    const VERSION: &'static str = "revision";
    fn validate_identity(
        &self,
        identity: &[String],
        version: u64,
    ) -> Result<(), AgentRegistryError> {
        self.validate()?;
        (self.id == identity[0] && self.revision == version)
            .then_some(())
            .ok_or_else(|| AgentRegistryError::Integrity("provider identity mismatch".into()))
    }
}
impl ValidateIdentity for ModelDefinition {
    const ID1: &'static str = "provider_id";
    const VERSION: &'static str = "revision";
    fn validate_identity(
        &self,
        identity: &[String],
        version: u64,
    ) -> Result<(), AgentRegistryError> {
        self.validate()?;
        (self.provider_id == identity[0]
            && self.model_id == identity[1]
            && self.revision == version)
            .then_some(())
            .ok_or_else(|| AgentRegistryError::Integrity("model identity mismatch".into()))
    }
}
type StoredRow = (String, String, i64, bool);
fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}
fn decode_stored<T: DeserializeOwned + ValidateIdentity>(
    row: StoredRow,
    identity: &[String],
    version: i64,
) -> Result<StoredRevision<T>, AgentRegistryError> {
    let (json, expected, created_at, active) = row;
    if digest(&json) != expected {
        return Err(AgentRegistryError::Integrity(
            "registry revision digest mismatch".into(),
        ));
    }
    let definition: T = serde_json::from_str(&json).map_err(AgentRegistryError::serde)?;
    let version = u64::try_from(version)
        .map_err(|_| AgentRegistryError::Integrity("negative registry version".into()))?;
    definition.validate_identity(identity, version)?;
    Ok(StoredRevision {
        definition,
        digest: expected,
        created_at,
        active,
    })
}

#[cfg(test)]
#[path = "../tests/unit/registry_domain_capability_tests.rs"]
mod capability_tests;
