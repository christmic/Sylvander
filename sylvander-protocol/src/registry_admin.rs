//! Transport-neutral administration contract for immutable runtime registries.
//!
//! The initial surface is deliberately read-only and Provider-only. Public
//! views contain digests for endpoint and credential-binding configuration,
//! never their configured values.

use serde::{Deserialize, Serialize};

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
}

impl RegistryAdminRequest {
    /// Validate transport-level identity and pagination invariants.
    pub fn validate(&self) -> Result<(), RegistryAdminError> {
        let (provider_id, revision) = match self {
            Self::InspectProviderRevision {
                provider_id,
                revision,
            } => (provider_id, Some(*revision)),
            Self::ListProviderRevisions { provider_id, .. } => (provider_id, None),
        };
        if provider_id.trim().is_empty() {
            return Err(RegistryAdminError::invalid_request(
                "provider identity must be set",
            ));
        }
        if revision == Some(0) {
            return Err(RegistryAdminError::invalid_request(
                "provider revision must be greater than zero",
            ));
        }
        if let Self::ListProviderRevisions { limit, .. } = self
            && !(1..=MAX_REGISTRY_REVISION_PAGE_SIZE).contains(limit)
        {
            return Err(RegistryAdminError::invalid_request(
                "revision page limit must be between 1 and 100",
            ));
        }
        Ok(())
    }
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
pub struct RegistryAdminError {
    pub code: RegistryAdminErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
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
}
