//! Content-safe registry administration failures.
//!
//! Errors expose stable typed identity and conflict metadata while credential
//! locators remain hashed and provider definitions remain outside the error
//! envelope.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryAdminError {
    pub code: RegistryAdminErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id_sha256: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Box<RegistryAdminErrorDetails>>,
}

impl RegistryAdminError {
    #[must_use]
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: RegistryAdminErrorCode::InvalidRequest,
            message: message.into(),
            provider_id: None,
            model_id: None,
            binding_id_sha256: None,
            revision: None,
            generation: None,
            details: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RegistryAdminErrorCode {
    Unauthorized,
    InvalidRequest,
    UnknownProvider,
    UnknownModel,
    UnknownCredentialBinding,
    UnknownRevision,
    UnknownGeneration,
    ProviderAlreadyExists,
    ModelAlreadyExists,
    ActiveRevisionConflict,
    NonSequentialRevision,
    RevisionCollision,
    InvalidRevisionRollback,
    CredentialAlreadyExists,
    ActiveGenerationConflict,
    NonSequentialGeneration,
    GenerationCollision,
    InvalidRollback,
    CredentialUnavailable,
    StorageUnavailable,
    IntegrityFailure,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistryAdminErrorDetails {
    ActiveRevisionConflict {
        expected_active_revision: u64,
        actual_active_revision: u64,
    },
    NonSequentialRevision {
        expected_revision: u64,
        actual_revision: u64,
    },
    RevisionCollision {
        revision: u64,
    },
    InvalidRevisionRollback {
        target_revision: u64,
        actual_active_revision: u64,
    },
    ActiveGenerationConflict {
        expected_active_generation: u64,
        actual_active_generation: u64,
    },
    NonSequentialGeneration {
        expected_generation: u64,
        actual_generation: u64,
    },
    GenerationCollision {
        generation: u64,
    },
    InvalidRollback {
        target_generation: u64,
        actual_active_generation: u64,
    },
}
