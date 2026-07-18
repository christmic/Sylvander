//! Redacted registry revision and credential-generation views.
//!
//! Administrative reads return digests and non-secret lifecycle metadata
//! rather than replaying write-only definitions or credential locators.

use serde::{Deserialize, Serialize};

use crate::ModelLifecycle;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderRevisionView {
    pub definition: RedactedProviderDefinition,
    pub digest_sha256: String,
    pub created_at_unix_secs: i64,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactedProviderDefinition {
    pub provider_id: String,
    pub revision: u64,
    pub kind: String,
    pub base_url_sha256: String,
    pub credential_binding_id_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRevisionView {
    pub definition: RedactedModelDefinition,
    pub digest_sha256: String,
    pub created_at_unix_secs: i64,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactedModelDefinition {
    pub provider_id: String,
    pub model_id: String,
    pub revision: u64,
    pub context_window: u32,
    pub max_output_tokens: u32,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub lifecycle: ModelLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CredentialGenerationView {
    pub binding_id_sha256: String,
    pub generation: u64,
    pub reference_kind: CredentialReferenceKind,
    pub reference_configured: bool,
    pub reference_digest_sha256: String,
    pub created_at_unix_secs: i64,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CredentialReferenceKind {
    Environment,
    File,
}
