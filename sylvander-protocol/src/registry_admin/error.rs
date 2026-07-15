use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryAdminError {
    pub code: RegistryAdminErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id_sha256: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<u64>,
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
    StorageUnavailable,
    IntegrityFailure,
    Internal,
}
