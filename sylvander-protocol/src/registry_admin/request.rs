use serde::{Deserialize, Serialize};

use super::RegistryAdminError;

pub const DEFAULT_REGISTRY_REVISION_PAGE_SIZE: u16 = 50;
pub const MAX_REGISTRY_REVISION_PAGE_SIZE: u16 = 100;

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
}

impl RegistryAdminRequest {
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
