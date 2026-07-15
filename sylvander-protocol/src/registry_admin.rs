//! Transport-neutral administration contract for immutable runtime registries.
//!
//! The initial surface is deliberately read-only. Public views contain
//! digests for sensitive Provider and pricing configuration, never their
//! configured values.

use serde::{Deserialize, Serialize};

use crate::ModelLifecycle;

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
}

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
pub struct RegistryAdminError {
    pub code: RegistryAdminErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

impl RegistryAdminError {
    #[must_use]
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: RegistryAdminErrorCode::InvalidRequest,
            message: message.into(),
            provider_id: None,
            model_id: None,
            revision: None,
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
    UnknownRevision,
    StorageUnavailable,
    IntegrityFailure,
    Internal,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn view() -> ProviderRevisionView {
        ProviderRevisionView {
            definition: RedactedProviderDefinition {
                provider_id: "alpha".into(),
                revision: 2,
                kind: "anthropic_compatible".into(),
                base_url_sha256: "base-digest".into(),
                credential_binding_id_sha256: "binding-digest".into(),
            },
            digest_sha256: "definition-digest".into(),
            created_at_unix_secs: 7,
            active: true,
        }
    }

    fn model_view() -> ModelRevisionView {
        ModelRevisionView {
            definition: RedactedModelDefinition {
                provider_id: "alpha".into(),
                model_id: "model-a".into(),
                revision: 3,
                context_window: 100_000,
                max_output_tokens: 4096,
                capabilities: vec!["tool_use".into()],
                lifecycle: ModelLifecycle::Active,
                pricing_sha256: Some("pricing-digest".into()),
            },
            digest_sha256: "model-digest".into(),
            created_at_unix_secs: 8,
            active: true,
        }
    }

    #[test]
    fn list_defaults_and_round_trips() {
        let request: RegistryAdminRequest = serde_json::from_value(json!({
            "operation": "list_provider_revisions",
            "provider_id": "alpha"
        }))
        .unwrap();
        assert_eq!(
            request,
            RegistryAdminRequest::ListProviderRevisions {
                provider_id: "alpha".into(),
                before_revision: None,
                limit: DEFAULT_REGISTRY_REVISION_PAGE_SIZE,
            }
        );
        request.validate().unwrap();
        assert_eq!(
            serde_json::from_value::<RegistryAdminRequest>(serde_json::to_value(&request).unwrap())
                .unwrap(),
            request
        );
    }

    #[test]
    fn validation_rejects_bad_identity_revision_and_page_limits() {
        for request in [
            RegistryAdminRequest::InspectProviderRevision {
                provider_id: " ".into(),
                revision: 1,
            },
            RegistryAdminRequest::InspectProviderRevision {
                provider_id: "alpha".into(),
                revision: 0,
            },
            RegistryAdminRequest::ListProviderRevisions {
                provider_id: "alpha".into(),
                before_revision: None,
                limit: 0,
            },
            RegistryAdminRequest::ListProviderRevisions {
                provider_id: "alpha".into(),
                before_revision: None,
                limit: 101,
            },
        ] {
            assert_eq!(
                request.validate().unwrap_err().code,
                RegistryAdminErrorCode::InvalidRequest
            );
        }
        assert!(
            serde_json::from_value::<RegistryAdminRequest>(json!({
                "operation": "list_provider_revisions",
                "provider_id": "alpha",
                "unexpected": true
            }))
            .is_err()
        );
    }

    #[test]
    fn provider_response_exposes_digests_not_configured_values() {
        let response = RegistryAdminResponse::Success {
            result: Box::new(RegistryAdminResult::ProviderRevisionInspected { revision: view() }),
        };
        let encoded = serde_json::to_value(response).unwrap();
        let definition = &encoded["result"]["revision"]["definition"];
        assert_eq!(definition["base_url_sha256"], "base-digest");
        assert_eq!(definition["credential_binding_id_sha256"], "binding-digest");
        assert!(definition.get("base_url").is_none());
        assert!(definition.get("credential_binding_id").is_none());
    }

    #[test]
    fn model_list_defaults_validates_and_round_trips() {
        let request: RegistryAdminRequest = serde_json::from_value(json!({
            "operation": "list_model_revisions",
            "provider_id": "alpha",
            "model_id": "model-a"
        }))
        .unwrap();
        assert_eq!(
            request,
            RegistryAdminRequest::ListModelRevisions {
                provider_id: "alpha".into(),
                model_id: "model-a".into(),
                before_revision: None,
                limit: DEFAULT_REGISTRY_REVISION_PAGE_SIZE,
            }
        );
        request.validate().unwrap();
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            serde_json::from_value::<RegistryAdminRequest>(encoded).unwrap(),
            request
        );
        for invalid in [
            RegistryAdminRequest::InspectModelRevision {
                provider_id: "alpha".into(),
                model_id: " ".into(),
                revision: 1,
            },
            RegistryAdminRequest::ListModelRevisions {
                provider_id: "alpha".into(),
                model_id: "model-a".into(),
                before_revision: Some(0),
                limit: 50,
            },
        ] {
            assert_eq!(
                invalid.validate().unwrap_err().code,
                RegistryAdminErrorCode::InvalidRequest
            );
        }
    }

    #[test]
    fn model_response_exposes_pricing_digest_not_pricing_configuration() {
        let response = RegistryAdminResponse::Success {
            result: Box::new(RegistryAdminResult::ModelRevisionInspected {
                revision: model_view(),
            }),
        };
        let encoded = serde_json::to_value(response).unwrap();
        let definition = &encoded["result"]["revision"]["definition"];
        assert_eq!(definition["pricing_sha256"], "pricing-digest");
        for field in [
            "pricing",
            "input_usd_micros_per_million",
            "output_usd_micros_per_million",
        ] {
            assert!(definition.get(field).is_none());
        }
    }

    #[test]
    fn model_error_identity_is_typed_and_legacy_errors_default_it() {
        let current: RegistryAdminError = serde_json::from_value(json!({
            "code": "unknown_model",
            "message": "model is unknown",
            "provider_id": "alpha",
            "model_id": "model-a"
        }))
        .unwrap();
        assert_eq!(current.model_id.as_deref(), Some("model-a"));
        let legacy: RegistryAdminError = serde_json::from_value(json!({
            "code": "unknown_revision",
            "message": "provider revision is unknown",
            "provider_id": "alpha",
            "revision": 2
        }))
        .unwrap();
        assert_eq!(legacy.model_id, None);
    }
}
