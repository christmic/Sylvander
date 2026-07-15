//! Privileged, read-only administration of immutable runtime registries.

use sha2::{Digest, Sha256};
use sylvander_protocol::{
    AuthenticatedPrincipal, ProviderRevisionView, RedactedProviderDefinition, RegistryAdminError,
    RegistryAdminErrorCode, RegistryAdminRequest, RegistryAdminResponse, RegistryAdminResult,
};

use crate::agent_admin::is_agent_administrator;
use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::provider_registry::ProviderRegistryError;
use crate::registry_domain::{ProviderDefinition, StoredRevision};

pub(crate) struct RegistryAdminService<'a> {
    registry: &'a AgentRegistry,
}

impl<'a> RegistryAdminService<'a> {
    #[must_use]
    pub(crate) const fn new(registry: &'a AgentRegistry) -> Self {
        Self { registry }
    }

    pub(crate) async fn dispatch(
        &self,
        principal: Option<&AuthenticatedPrincipal>,
        request: RegistryAdminRequest,
    ) -> RegistryAdminResponse {
        if !is_registry_administrator(principal) {
            return failure(error(
                RegistryAdminErrorCode::Unauthorized,
                "registry administration requires an administrator",
                None,
                None,
            ));
        }
        if let Err(error) = request.validate() {
            return failure(error);
        }
        match request {
            RegistryAdminRequest::InspectProviderRevision {
                provider_id,
                revision,
            } => self.inspect(provider_id, revision).await,
            RegistryAdminRequest::ListProviderRevisions {
                provider_id,
                before_revision,
                limit,
            } => self.list(provider_id, before_revision, limit).await,
        }
    }

    async fn inspect(&self, provider_id: String, revision: u64) -> RegistryAdminResponse {
        match self
            .registry
            .load_provider_revision(&provider_id, revision)
            .await
        {
            Ok(Some(stored)) => success(RegistryAdminResult::ProviderRevisionInspected {
                revision: redact_provider_revision(&stored),
            }),
            Ok(None) => failure(error(
                RegistryAdminErrorCode::UnknownRevision,
                "provider revision is unknown",
                Some(provider_id),
                Some(revision),
            )),
            Err(source) => failure(map_registry_error(source, provider_id, Some(revision))),
        }
    }

    async fn list(
        &self,
        provider_id: String,
        before: Option<u64>,
        limit: u16,
    ) -> RegistryAdminResponse {
        match self.registry.inspect_provider(&provider_id).await {
            Ok(stored) if stored.is_empty() => failure(error(
                RegistryAdminErrorCode::UnknownProvider,
                "provider is unknown",
                Some(provider_id),
                None,
            )),
            Ok(stored) => {
                let Some(active_revision) = stored
                    .iter()
                    .find(|revision| revision.active)
                    .map(|revision| revision.definition.revision)
                else {
                    return failure(error(
                        RegistryAdminErrorCode::IntegrityFailure,
                        "provider registry integrity check failed",
                        Some(provider_id),
                        None,
                    ));
                };
                let mut eligible = stored.iter().filter(|revision| {
                    before.is_none_or(|value| revision.definition.revision < value)
                });
                let revisions = eligible
                    .by_ref()
                    .take(usize::from(limit))
                    .map(redact_provider_revision)
                    .collect::<Vec<_>>();
                let next_before_revision = eligible
                    .next()
                    .and_then(|_| revisions.last().map(|item| item.definition.revision));
                success(RegistryAdminResult::ProviderRevisionsListed {
                    provider_id,
                    active_revision,
                    revisions,
                    next_before_revision,
                })
            }
            Err(source) => failure(map_provider_error(source, provider_id)),
        }
    }
}

#[must_use]
pub(crate) fn is_registry_administrator(principal: Option<&AuthenticatedPrincipal>) -> bool {
    is_agent_administrator(principal)
}

#[must_use]
pub(crate) fn redact_provider_revision(
    revision: &StoredRevision<ProviderDefinition>,
) -> ProviderRevisionView {
    ProviderRevisionView {
        definition: RedactedProviderDefinition {
            provider_id: revision.definition.id.clone(),
            revision: revision.definition.revision,
            kind: revision.definition.kind.clone(),
            base_url_sha256: sha256(&revision.definition.base_url),
            credential_binding_id_sha256: sha256(&revision.definition.credential_binding_id),
        },
        digest_sha256: revision.digest.clone(),
        created_at_unix_secs: revision.created_at,
        active: revision.active,
    }
}

fn map_provider_error(source: ProviderRegistryError, provider_id: String) -> RegistryAdminError {
    match source {
        ProviderRegistryError::UnknownProvider(_) => error(
            RegistryAdminErrorCode::UnknownProvider,
            "provider is unknown",
            Some(provider_id),
            None,
        ),
        ProviderRegistryError::UnknownRevision { revision, .. } => error(
            RegistryAdminErrorCode::UnknownRevision,
            "provider revision is unknown",
            Some(provider_id),
            Some(revision),
        ),
        ProviderRegistryError::Registry(source) => map_registry_error(source, provider_id, None),
        _ => error(
            RegistryAdminErrorCode::Internal,
            "provider registry operation failed",
            Some(provider_id),
            None,
        ),
    }
}

fn map_registry_error(
    source: AgentRegistryError,
    provider_id: String,
    revision: Option<u64>,
) -> RegistryAdminError {
    let (code, message) = match source {
        AgentRegistryError::Storage(_) | AgentRegistryError::Task(_) => (
            RegistryAdminErrorCode::StorageUnavailable,
            "provider registry is unavailable",
        ),
        AgentRegistryError::Serialization(_) | AgentRegistryError::Integrity(_) => (
            RegistryAdminErrorCode::IntegrityFailure,
            "provider registry integrity check failed",
        ),
        AgentRegistryError::Invalid(_) => (
            RegistryAdminErrorCode::InvalidRequest,
            "provider revision is invalid",
        ),
        _ => (
            RegistryAdminErrorCode::Internal,
            "provider registry operation failed",
        ),
    };
    error(code, message, Some(provider_id), revision)
}

fn success(result: RegistryAdminResult) -> RegistryAdminResponse {
    RegistryAdminResponse::Success {
        result: Box::new(result),
    }
}

fn failure(error: RegistryAdminError) -> RegistryAdminResponse {
    RegistryAdminResponse::Error { error }
}

fn error(
    code: RegistryAdminErrorCode,
    message: &'static str,
    provider_id: Option<String>,
    revision: Option<u64>,
) -> RegistryAdminError {
    RegistryAdminError {
        code,
        message: message.into(),
        provider_id,
        revision,
    }
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use sylvander_protocol::{AuthenticationMethod, RegistryAdminErrorCode};

    use super::*;
    use crate::config::SecretRef;
    use crate::registry_domain::CredentialBindingRevision;

    const RAW_URL: &str = "https://user:RAW_URL_SECRET@example.invalid/path?token=leak";
    const RAW_BINDING: &str = "RAW_BINDING_SECRET";

    fn provider(revision: u64, base_url: &str) -> ProviderDefinition {
        ProviderDefinition {
            id: "alpha".into(),
            revision,
            kind: "anthropic_compatible".into(),
            base_url: base_url.into(),
            credential_binding_id: RAW_BINDING.into(),
        }
    }

    async fn registry() -> AgentRegistry {
        let registry = AgentRegistry::open(":memory:").await.unwrap();
        registry
            .seed_credential(CredentialBindingRevision {
                binding_id: RAW_BINDING.into(),
                generation: 1,
                reference: SecretRef::Env {
                    name: "UNRESOLVED_TEST_REFERENCE".into(),
                },
            })
            .await
            .unwrap();
        registry.seed_provider(provider(1, RAW_URL)).await.unwrap();
        registry
    }

    fn admin() -> AuthenticatedPrincipal {
        let mut principal =
            AuthenticatedPrincipal::user("operator", AuthenticationMethod::Internal);
        principal.roles.push("admin".into());
        principal
    }

    #[tokio::test]
    async fn exact_inspection_stays_pinned_and_redacted_after_head_moves() {
        let registry = registry().await;
        registry
            .stage_provider(1, provider(2, "https://new.invalid"))
            .await
            .unwrap();
        registry.activate_provider("alpha", 2, 1).await.unwrap();
        let response = RegistryAdminService::new(&registry)
            .dispatch(
                Some(&admin()),
                RegistryAdminRequest::InspectProviderRevision {
                    provider_id: "alpha".into(),
                    revision: 1,
                },
            )
            .await;
        let encoded = serde_json::to_string(&response).unwrap();
        let debug = format!("{response:?}");
        for marker in [RAW_URL, "RAW_URL_SECRET", RAW_BINDING] {
            assert!(!encoded.contains(marker));
            assert!(!debug.contains(marker));
        }
        let RegistryAdminResponse::Success { result } = response else {
            panic!("expected success");
        };
        let RegistryAdminResult::ProviderRevisionInspected { revision } = *result else {
            panic!("expected inspection");
        };
        assert_eq!(revision.definition.revision, 1);
        assert!(!revision.active);
        assert_eq!(revision.definition.base_url_sha256, sha256(RAW_URL));
        assert_eq!(
            revision.definition.credential_binding_id_sha256,
            sha256(RAW_BINDING)
        );
    }

    #[tokio::test]
    async fn list_is_descending_paginated_and_reports_active_revision() {
        let registry = registry().await;
        for revision in 2..=3 {
            registry
                .stage_provider(
                    1,
                    provider(revision, &format!("https://v{revision}.invalid")),
                )
                .await
                .unwrap();
        }
        registry.activate_provider("alpha", 3, 1).await.unwrap();
        let service = RegistryAdminService::new(&registry);
        let first = service
            .dispatch(
                Some(&admin()),
                RegistryAdminRequest::ListProviderRevisions {
                    provider_id: "alpha".into(),
                    before_revision: None,
                    limit: 2,
                },
            )
            .await;
        let RegistryAdminResponse::Success { result } = first else {
            panic!("expected first page");
        };
        let RegistryAdminResult::ProviderRevisionsListed {
            active_revision,
            revisions,
            next_before_revision,
            ..
        } = *result
        else {
            panic!("expected list");
        };
        assert_eq!(active_revision, 3);
        assert_eq!(
            revisions
                .iter()
                .map(|item| item.definition.revision)
                .collect::<Vec<_>>(),
            [3, 2]
        );
        assert_eq!(next_before_revision, Some(2));

        let second = service
            .dispatch(
                Some(&admin()),
                RegistryAdminRequest::ListProviderRevisions {
                    provider_id: "alpha".into(),
                    before_revision: next_before_revision,
                    limit: 2,
                },
            )
            .await;
        let RegistryAdminResponse::Success { result } = second else {
            panic!("expected second page");
        };
        let RegistryAdminResult::ProviderRevisionsListed {
            revisions,
            next_before_revision,
            ..
        } = *result
        else {
            panic!("expected list");
        };
        assert_eq!(revisions[0].definition.revision, 1);
        assert_eq!(next_before_revision, None);
    }

    #[tokio::test]
    async fn unknown_and_unauthorized_fail_with_fixed_typed_errors() {
        let registry = registry().await;
        let service = RegistryAdminService::new(&registry);
        let request = RegistryAdminRequest::InspectProviderRevision {
            provider_id: "missing".into(),
            revision: 7,
        };
        let unauthorized = service.dispatch(None, request.clone()).await;
        let unknown = service.dispatch(Some(&admin()), request).await;
        assert!(matches!(
            unauthorized,
            RegistryAdminResponse::Error { error }
                if error.code == RegistryAdminErrorCode::Unauthorized
                    && error.message == "registry administration requires an administrator"
        ));
        assert!(matches!(
            unknown,
            RegistryAdminResponse::Error { error }
                if error.code == RegistryAdminErrorCode::UnknownRevision
                    && error.message == "provider revision is unknown"
        ));
    }
}
