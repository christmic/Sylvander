//! Version-gated registry inspection and lifecycle mutation requests.
//!
//! The request envelope is deliberately strict: unknown fields are rejected,
//! pagination is bounded, and mutations carry expected active revisions so the
//! Runtime can detect conflicting administrators.

use serde::{Deserialize, Serialize};

use super::{
    CredentialSecretReferenceDraft, ModelDefinitionDraft, ModelLifecycleDraft,
    ProviderDefinitionDraft, RegistryAdminError,
};

pub const DEFAULT_REGISTRY_REVISION_PAGE_SIZE: u16 = 50;
pub const MAX_REGISTRY_REVISION_PAGE_SIZE: u16 = 100;
pub const REGISTRY_ADMIN_READ_MIN_UI_PROTOCOL_VERSION: u16 = crate::UI_PROTOCOL_VERSION;
pub const REGISTRY_ADMIN_MUTATION_MIN_UI_PROTOCOL_VERSION: u16 = crate::UI_PROTOCOL_VERSION;

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
    CreateProvider {
        provider_id: String,
        definition: ProviderDefinitionDraft,
    },
    StageProviderRevision {
        provider_id: String,
        revision: u64,
        expected_active_revision: u64,
        definition: ProviderDefinitionDraft,
    },
    ActivateProviderRevision {
        provider_id: String,
        revision: u64,
        expected_active_revision: u64,
    },
    RollbackProviderRevision {
        provider_id: String,
        target_revision: u64,
        expected_active_revision: u64,
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
    CreateModel {
        provider_id: String,
        model_id: String,
        definition: ModelDefinitionDraft,
    },
    StageModelRevision {
        provider_id: String,
        model_id: String,
        revision: u64,
        expected_active_revision: u64,
        definition: ModelDefinitionDraft,
    },
    ActivateModelRevision {
        provider_id: String,
        model_id: String,
        revision: u64,
        expected_active_revision: u64,
    },
    RollbackModelRevision {
        provider_id: String,
        model_id: String,
        target_revision: u64,
        expected_active_revision: u64,
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
    /// All registry operations require the single current UI protocol.
    #[must_use]
    pub const fn minimum_ui_protocol_version(&self) -> u16 {
        match self {
            Self::InspectProviderRevision { .. }
            | Self::ListProviderRevisions { .. }
            | Self::InspectModelRevision { .. }
            | Self::ListModelRevisions { .. }
            | Self::InspectCredentialGeneration { .. }
            | Self::ListCredentialGenerations { .. } => REGISTRY_ADMIN_READ_MIN_UI_PROTOCOL_VERSION,
            Self::CreateProvider { .. }
            | Self::StageProviderRevision { .. }
            | Self::ActivateProviderRevision { .. }
            | Self::RollbackProviderRevision { .. }
            | Self::CreateModel { .. }
            | Self::StageModelRevision { .. }
            | Self::ActivateModelRevision { .. }
            | Self::RollbackModelRevision { .. }
            | Self::CreateCredentialBinding { .. }
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
            Self::CreateProvider {
                provider_id,
                definition,
            } => validate_provider(provider_id).and_then(|()| validate_provider_draft(definition)),
            Self::StageProviderRevision {
                provider_id,
                revision,
                expected_active_revision,
                definition,
            } => validate_provider(provider_id)
                .and_then(|()| validate_revision(*revision))
                .and_then(|()| validate_revision(*expected_active_revision))
                .and_then(|()| validate_provider_draft(definition)),
            Self::ActivateProviderRevision {
                provider_id,
                revision,
                expected_active_revision,
            } => validate_provider(provider_id)
                .and_then(|()| validate_revision(*revision))
                .and_then(|()| validate_revision(*expected_active_revision)),
            Self::RollbackProviderRevision {
                provider_id,
                target_revision,
                expected_active_revision,
            } => validate_provider(provider_id)
                .and_then(|()| validate_revision(*target_revision))
                .and_then(|()| validate_revision(*expected_active_revision)),
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
            Self::CreateModel {
                provider_id,
                model_id,
                definition,
            } => validate_provider(provider_id)
                .and_then(|()| validate_model(model_id))
                .and_then(|()| validate_model_draft(definition)),
            Self::StageModelRevision {
                provider_id,
                model_id,
                revision,
                expected_active_revision,
                definition,
            } => validate_provider(provider_id)
                .and_then(|()| validate_model(model_id))
                .and_then(|()| validate_revision(*revision))
                .and_then(|()| validate_revision(*expected_active_revision))
                .and_then(|()| validate_model_draft(definition)),
            Self::ActivateModelRevision {
                provider_id,
                model_id,
                revision,
                expected_active_revision,
            } => validate_provider(provider_id)
                .and_then(|()| validate_model(model_id))
                .and_then(|()| validate_revision(*revision))
                .and_then(|()| validate_revision(*expected_active_revision)),
            Self::RollbackModelRevision {
                provider_id,
                model_id,
                target_revision,
                expected_active_revision,
            } => validate_provider(provider_id)
                .and_then(|()| validate_model(model_id))
                .and_then(|()| validate_revision(*target_revision))
                .and_then(|()| validate_revision(*expected_active_revision)),
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

fn validate_provider_draft(definition: &ProviderDefinitionDraft) -> Result<(), RegistryAdminError> {
    definition.is_configured().then_some(()).ok_or_else(|| {
        RegistryAdminError::invalid_request("provider definition must be configured")
    })
}

fn validate_model_draft(definition: &ModelDefinitionDraft) -> Result<(), RegistryAdminError> {
    if definition.context_window == 0 || definition.max_output_tokens == 0 {
        return Err(RegistryAdminError::invalid_request(
            "model token limits must be positive",
        ));
    }
    if definition
        .capabilities
        .iter()
        .any(|capability| capability.trim().is_empty())
    {
        return Err(RegistryAdminError::invalid_request(
            "model capabilities must not be blank",
        ));
    }
    let mut capabilities = std::collections::BTreeSet::new();
    if definition
        .capabilities
        .iter()
        .any(|capability| !capabilities.insert(capability))
    {
        return Err(RegistryAdminError::invalid_request(
            "model capabilities must not contain duplicates",
        ));
    }
    if matches!(
        &definition.lifecycle,
        ModelLifecycleDraft::Deprecated { replacement: Some(replacement) }
            if replacement.trim().is_empty()
    ) {
        return Err(RegistryAdminError::invalid_request(
            "deprecated model replacement must be set",
        ));
    }
    Ok(())
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
