use serde::{Deserialize, Serialize};

use super::{CredentialSecretReferenceDraft, RegistryAdminError};

pub const DEFAULT_REGISTRY_REVISION_PAGE_SIZE: u16 = 50;
pub const MAX_REGISTRY_REVISION_PAGE_SIZE: u16 = 100;
pub const REGISTRY_ADMIN_READ_MIN_UI_PROTOCOL_VERSION: u16 = 2;
pub const REGISTRY_ADMIN_MUTATION_MIN_UI_PROTOCOL_VERSION: u16 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistryAdminRequest {
    InspectProviderRevision {
        provider_id: String,
        revision: u64,
    },
    ListProviderRevisions {
        provider_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_revision: Option<u64>,
        #[serde(default = "default_page_size")]
        limit: u16,
    },
    InspectModelRevision {
        provider_id: String,
        model_id: String,
        revision: u64,
    },
    ListModelRevisions {
        provider_id: String,
        model_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_revision: Option<u64>,
        #[serde(default = "default_page_size")]
        limit: u16,
    },
    InspectCredentialGeneration {
        binding_id: String,
        generation: u64,
    },
    ListCredentialGenerations {
        binding_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        before_generation: Option<u64>,
        #[serde(default = "default_page_size")]
        limit: u16,
    },
    CreateCredentialBinding {
        binding_id: String,
        reference: CredentialSecretReferenceDraft,
    },
    StageCredentialGeneration {
        binding_id: String,
        generation: u64,
        expected_active_generation: u64,
        reference: CredentialSecretReferenceDraft,
    },
    ActivateCredentialGeneration {
        binding_id: String,
        generation: u64,
        expected_active_generation: u64,
    },
    RollbackCredentialGeneration {
        binding_id: String,
        target_generation: u64,
        expected_active_generation: u64,
    },
}

impl RegistryAdminRequest {
    /// Old read operations are available in v2; state mutations require v3.
    #[must_use]
    pub const fn minimum_ui_protocol_version(&self) -> u16 {
        match self {
            Self::InspectProviderRevision { .. }
            | Self::ListProviderRevisions { .. }
            | Self::InspectModelRevision { .. }
            | Self::ListModelRevisions { .. }
            | Self::InspectCredentialGeneration { .. }
            | Self::ListCredentialGenerations { .. } => REGISTRY_ADMIN_READ_MIN_UI_PROTOCOL_VERSION,
            Self::CreateCredentialBinding { .. }
            | Self::StageCredentialGeneration { .. }
            | Self::ActivateCredentialGeneration { .. }
            | Self::RollbackCredentialGeneration { .. } => {
                REGISTRY_ADMIN_MUTATION_MIN_UI_PROTOCOL_VERSION
            }
        }
    }

    /// Validate transport-level identity and pagination invariants.
    pub fn validate(&self) -> Result<(), RegistryAdminError> {
        match self {
            Self::InspectProviderRevision {
                provider_id,
                revision,
            } => validate_provider(provider_id).and_then(|()| validate_revision(*revision)),
            Self::ListProviderRevisions {
                provider_id,
                before_revision,
                limit,
            } => validate_provider(provider_id)
                .and_then(|()| validate_page(*before_revision, *limit)),
            Self::InspectModelRevision {
                provider_id,
                model_id,
                revision,
            } => validate_provider(provider_id)
                .and_then(|()| validate_model(model_id))
                .and_then(|()| validate_revision(*revision)),
            Self::ListModelRevisions {
                provider_id,
                model_id,
                before_revision,
                limit,
            } => validate_provider(provider_id)
                .and_then(|()| validate_model(model_id))
                .and_then(|()| validate_page(*before_revision, *limit)),
            Self::InspectCredentialGeneration {
                binding_id,
                generation,
            } => validate_binding(binding_id).and_then(|()| validate_revision(*generation)),
            Self::ListCredentialGenerations {
                binding_id,
                before_generation,
                limit,
            } => validate_binding(binding_id)
                .and_then(|()| validate_page(*before_generation, *limit)),
            Self::CreateCredentialBinding {
                binding_id,
                reference,
            } => validate_binding(binding_id).and_then(|()| validate_reference(reference)),
            Self::StageCredentialGeneration {
                binding_id,
                generation,
                expected_active_generation,
                reference,
            } => validate_binding(binding_id)
                .and_then(|()| validate_generation(*generation))
                .and_then(|()| validate_generation(*expected_active_generation))
                .and_then(|()| validate_reference(reference)),
            Self::ActivateCredentialGeneration {
                binding_id,
                generation,
                expected_active_generation,
            } => validate_binding(binding_id)
                .and_then(|()| validate_generation(*generation))
                .and_then(|()| validate_generation(*expected_active_generation)),
            Self::RollbackCredentialGeneration {
                binding_id,
                target_generation,
                expected_active_generation,
            } => validate_binding(binding_id)
                .and_then(|()| validate_generation(*target_generation))
                .and_then(|()| validate_generation(*expected_active_generation)),
        }
    }
}

fn validate_provider(provider_id: &str) -> Result<(), RegistryAdminError> {
    (!provider_id.trim().is_empty())
        .then_some(())
        .ok_or_else(|| RegistryAdminError::invalid_request("provider identity must be set"))
}

fn validate_model(model_id: &str) -> Result<(), RegistryAdminError> {
    (!model_id.trim().is_empty())
        .then_some(())
        .ok_or_else(|| RegistryAdminError::invalid_request("model identity must be set"))
}

fn validate_binding(binding_id: &str) -> Result<(), RegistryAdminError> {
    (!binding_id.trim().is_empty())
        .then_some(())
        .ok_or_else(|| RegistryAdminError::invalid_request("credential binding must be set"))
}

fn validate_revision(revision: u64) -> Result<(), RegistryAdminError> {
    (revision > 0)
        .then_some(())
        .ok_or_else(|| RegistryAdminError::invalid_request("revision must be greater than zero"))
}

fn validate_generation(generation: u64) -> Result<(), RegistryAdminError> {
    (generation > 0).then_some(()).ok_or_else(|| {
        RegistryAdminError::invalid_request("credential generation must be greater than zero")
    })
}

fn validate_reference(
    reference: &CredentialSecretReferenceDraft,
) -> Result<(), RegistryAdminError> {
    reference.is_configured().then_some(()).ok_or_else(|| {
        RegistryAdminError::invalid_request("credential reference must be configured")
    })
}

fn validate_page(before: Option<u64>, limit: u16) -> Result<(), RegistryAdminError> {
    if before == Some(0) {
        return Err(RegistryAdminError::invalid_request(
            "revision cursor must be greater than zero",
        ));
    }
    (1..=MAX_REGISTRY_REVISION_PAGE_SIZE)
        .contains(&limit)
        .then_some(())
        .ok_or_else(|| {
            RegistryAdminError::invalid_request("revision page limit must be between 1 and 100")
        })
}

const fn default_page_size() -> u16 {
    DEFAULT_REGISTRY_REVISION_PAGE_SIZE
}
