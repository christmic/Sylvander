//! Transport-neutral administration contract for immutable runtime registries.
//!
//! The initial surface is deliberately read-only. Public views contain
//! digests for sensitive Provider and pricing configuration, never their
//! configured values.

mod draft;
mod error;
mod request;
mod view;

pub use draft::{
    CredentialSecretReferenceDraft, ModelDefinitionDraft, ModelLifecycleDraft, ModelPricingDraft,
    ProviderDefinitionDraft,
};
pub use error::{RegistryAdminError, RegistryAdminErrorCode, RegistryAdminErrorDetails};
pub use request::{
    DEFAULT_REGISTRY_REVISION_PAGE_SIZE, MAX_REGISTRY_REVISION_PAGE_SIZE,
    REGISTRY_ADMIN_MUTATION_MIN_UI_PROTOCOL_VERSION, REGISTRY_ADMIN_READ_MIN_UI_PROTOCOL_VERSION,
    RegistryAdminRequest,
};
pub use view::{
    CredentialGenerationView, CredentialReferenceKind, ModelRevisionView, ProviderRevisionView,
    RedactedModelDefinition, RedactedProviderDefinition,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistryAdminResponse {
    Success { result: Box<RegistryAdminResult> },
    Error { error: RegistryAdminError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistryAdminResult {
    ProviderRevisionInspected {
        revision: ProviderRevisionView,
    },
    ProviderRevisionsListed {
        provider_id: String,
        active_revision: u64,
        revisions: Vec<ProviderRevisionView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_before_revision: Option<u64>,
    },
    ProviderCreated {
        revision: ProviderRevisionView,
    },
    ProviderRevisionStaged {
        revision: ProviderRevisionView,
    },
    ProviderRevisionActivated {
        revision: ProviderRevisionView,
    },
    ProviderRevisionRolledBack {
        revision: ProviderRevisionView,
    },
    ModelRevisionInspected {
        revision: ModelRevisionView,
    },
    ModelRevisionsListed {
        provider_id: String,
        model_id: String,
        active_revision: u64,
        revisions: Vec<ModelRevisionView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_before_revision: Option<u64>,
    },
    ModelCreated {
        revision: ModelRevisionView,
    },
    ModelRevisionStaged {
        revision: ModelRevisionView,
    },
    ModelRevisionActivated {
        revision: ModelRevisionView,
    },
    ModelRevisionRolledBack {
        revision: ModelRevisionView,
    },
    CredentialGenerationInspected {
        generation: CredentialGenerationView,
    },
    CredentialGenerationsListed {
        binding_id_sha256: String,
        active_generation: u64,
        generations: Vec<CredentialGenerationView>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_before_generation: Option<u64>,
    },
    CredentialBindingCreated {
        generation: CredentialGenerationView,
    },
    CredentialGenerationStaged {
        generation: CredentialGenerationView,
    },
    CredentialGenerationActivated {
        binding_id_sha256: String,
        active_generation: u64,
    },
    CredentialGenerationRolledBack {
        binding_id_sha256: String,
        active_generation: u64,
    },
}

#[cfg(test)]
#[path = "../../tests/unit/registry_admin.rs"]
mod tests;
