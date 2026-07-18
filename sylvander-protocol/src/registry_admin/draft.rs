//! Write-only drafts accepted by registry lifecycle mutations.
//!
//! These wire types accept declarative provider, model, pricing, and secret
//! reference metadata. Their custom `Debug` implementations never reveal the
//! submitted configuration.

use serde::{Deserialize, Serialize};

/// Write-only Provider configuration accepted by lifecycle mutations.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProviderDefinitionDraft {
    pub kind: String,
    pub base_url: String,
    pub credential_binding_id: String,
}

impl ProviderDefinitionDraft {
    pub(super) fn is_configured(&self) -> bool {
        [&self.kind, &self.base_url, &self.credential_binding_id]
            .into_iter()
            .all(|value| !value.trim().is_empty())
    }
}

impl std::fmt::Debug for ProviderDefinitionDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ProviderDefinitionDraft([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelDefinitionDraft {
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub capabilities: Vec<String>,
    pub lifecycle: ModelLifecycleDraft,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<ModelPricingDraft>,
}

impl std::fmt::Debug for ModelDefinitionDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ModelDefinitionDraft([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelLifecycleDraft {
    Active {},
    Deprecated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replacement: Option<String>,
    },
}

impl std::fmt::Debug for ModelLifecycleDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ModelLifecycleDraft([REDACTED])")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelPricingDraft {
    pub input_usd_micros_per_million: u64,
    pub output_usd_micros_per_million: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_usd_micros_per_million: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_usd_micros_per_million: Option<u64>,
}

impl std::fmt::Debug for ModelPricingDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ModelPricingDraft([REDACTED])")
    }
}

/// Write-only credential locator. Secret values are never accepted by this contract.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialSecretReferenceDraft {
    Environment { name: String },
    File { path: String },
}

impl CredentialSecretReferenceDraft {
    pub(super) fn is_configured(&self) -> bool {
        match self {
            Self::Environment { name } => !name.trim().is_empty(),
            Self::File { path } => !path.trim().is_empty(),
        }
    }
}

impl std::fmt::Debug for CredentialSecretReferenceDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialSecretReferenceDraft([REDACTED])")
    }
}
