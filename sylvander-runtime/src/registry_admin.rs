//! Privileged, read-only administration of immutable runtime registries.

use sha2::{Digest, Sha256};
use sylvander_protocol::{
    AuthenticatedPrincipal, CredentialGenerationView, CredentialReferenceKind,
    CredentialSecretReferenceDraft, ModelRevisionView, ProviderRevisionView,
    RedactedModelDefinition, RedactedProviderDefinition, RegistryAdminError,
    RegistryAdminErrorCode, RegistryAdminErrorDetails, RegistryAdminRequest, RegistryAdminResponse,
    RegistryAdminResult,
};

use crate::agent_admin::is_agent_administrator;
use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::config::SecretRef;
use crate::credential_registry::{CredentialRegistryError, CredentialSecretResolver};
use crate::model_registry::ModelRegistryError;
use crate::provider_registry::ProviderRegistryError;
use crate::registry_domain::{
    CredentialBindingView, ModelDefinition, ProviderDefinition, SecretReferenceKind, StoredRevision,
};

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
            RegistryAdminRequest::InspectModelRevision {
                provider_id,
                model_id,
                revision,
            } => self.inspect_model(provider_id, model_id, revision).await,
            RegistryAdminRequest::ListModelRevisions {
                provider_id,
                model_id,
                before_revision,
                limit,
            } => {
                self.list_models(provider_id, model_id, before_revision, limit)
                    .await
            }
            RegistryAdminRequest::InspectCredentialGeneration {
                binding_id,
                generation,
            } => self.inspect_credential(binding_id, generation).await,
            RegistryAdminRequest::ListCredentialGenerations {
                binding_id,
                before_generation,
                limit,
            } => {
                self.list_credentials(binding_id, before_generation, limit)
                    .await
            }
            RegistryAdminRequest::CreateCredentialBinding { .. }
            | RegistryAdminRequest::StageCredentialGeneration { .. }
            | RegistryAdminRequest::ActivateCredentialGeneration { .. }
            | RegistryAdminRequest::RollbackCredentialGeneration { .. } => failure(error(
                RegistryAdminErrorCode::Internal,
                "credential mutation dispatcher is unavailable",
                None,
                None,
            )),
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
        match self
            .registry
            .inspect_provider_page(&provider_id, before, limit)
            .await
        {
            Ok(page) => success(RegistryAdminResult::ProviderRevisionsListed {
                provider_id,
                active_revision: page.active_revision,
                revisions: page
                    .revisions
                    .iter()
                    .map(redact_provider_revision)
                    .collect(),
                next_before_revision: page.next_before_revision,
            }),
            Err(source) => failure(map_provider_error(source, provider_id)),
        }
    }

    async fn inspect_model(
        &self,
        provider_id: String,
        model_id: String,
        revision: u64,
    ) -> RegistryAdminResponse {
        match self
            .registry
            .load_model_revision(&provider_id, &model_id, revision)
            .await
        {
            Ok(Some(stored)) => match redact_model_revision(&stored) {
                Ok(revision) => success(RegistryAdminResult::ModelRevisionInspected { revision }),
                Err(source) => failure(map_model_storage_error(
                    source,
                    provider_id,
                    model_id,
                    Some(revision),
                )),
            },
            Ok(None) => failure(model_error(
                RegistryAdminErrorCode::UnknownRevision,
                "model revision is unknown",
                provider_id,
                model_id,
                Some(revision),
            )),
            Err(source) => failure(map_model_storage_error(
                source,
                provider_id,
                model_id,
                Some(revision),
            )),
        }
    }

    async fn list_models(
        &self,
        provider_id: String,
        model_id: String,
        before: Option<u64>,
        limit: u16,
    ) -> RegistryAdminResponse {
        match self
            .registry
            .inspect_model_page((&provider_id, &model_id), before, limit)
            .await
        {
            Ok(page) => {
                let revisions = page
                    .revisions
                    .iter()
                    .map(redact_model_revision)
                    .collect::<Result<Vec<_>, _>>();
                let revisions = match revisions {
                    Ok(revisions) => revisions,
                    Err(source) => {
                        return failure(map_model_storage_error(
                            source,
                            provider_id,
                            model_id,
                            None,
                        ));
                    }
                };
                success(RegistryAdminResult::ModelRevisionsListed {
                    provider_id,
                    model_id,
                    active_revision: page.active_revision,
                    revisions,
                    next_before_revision: page.next_before_revision,
                })
            }
            Err(source) => failure(map_model_error(source, provider_id, model_id)),
        }
    }

    async fn inspect_credential(
        &self,
        binding_id: String,
        generation: u64,
    ) -> RegistryAdminResponse {
        match self
            .registry
            .inspect_credential_revision(&binding_id, generation)
            .await
        {
            Ok(Some(view)) => success(RegistryAdminResult::CredentialGenerationInspected {
                generation: redact_credential_generation(&view),
            }),
            Ok(None) => match self
                .registry
                .inspect_credential_page(&binding_id, None, 1)
                .await
            {
                Ok(_) => failure(credential_error(
                    RegistryAdminErrorCode::UnknownGeneration,
                    "credential generation is unknown",
                    &binding_id,
                    Some(generation),
                )),
                Err(source) => failure(map_credential_error(source, &binding_id, None)),
            },
            Err(source) => failure(map_credential_storage_error(
                source,
                &binding_id,
                Some(generation),
            )),
        }
    }

    async fn list_credentials(
        &self,
        binding_id: String,
        before: Option<u64>,
        limit: u16,
    ) -> RegistryAdminResponse {
        match self
            .registry
            .inspect_credential_page(&binding_id, before, limit)
            .await
        {
            Ok(page) => success(RegistryAdminResult::CredentialGenerationsListed {
                binding_id_sha256: sha256(&binding_id),
                active_generation: page.active_generation,
                generations: page
                    .generations
                    .iter()
                    .map(redact_credential_generation)
                    .collect(),
                next_before_generation: page.next_before_generation,
            }),
            Err(source) => failure(map_credential_error(source, &binding_id, None)),
        }
    }
}

pub(crate) struct CredentialRegistryMutationService<'a> {
    registry: &'a AgentRegistry,
    resolver: &'a dyn CredentialSecretResolver,
}

impl<'a> CredentialRegistryMutationService<'a> {
    #[must_use]
    pub(crate) const fn new(
        registry: &'a AgentRegistry,
        resolver: &'a dyn CredentialSecretResolver,
    ) -> Self {
        Self { registry, resolver }
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
            RegistryAdminRequest::CreateCredentialBinding {
                binding_id,
                reference,
            } => self.create(binding_id, reference).await,
            RegistryAdminRequest::StageCredentialGeneration {
                binding_id,
                generation,
                expected_active_generation,
                reference,
            } => {
                self.stage(
                    binding_id,
                    generation,
                    expected_active_generation,
                    reference,
                )
                .await
            }
            RegistryAdminRequest::ActivateCredentialGeneration {
                binding_id,
                generation,
                expected_active_generation,
            } => {
                self.activate(binding_id, generation, expected_active_generation)
                    .await
            }
            RegistryAdminRequest::RollbackCredentialGeneration {
                binding_id,
                target_generation,
                expected_active_generation,
            } => {
                self.rollback(binding_id, target_generation, expected_active_generation)
                    .await
            }
            _ => failure(error(
                RegistryAdminErrorCode::InvalidRequest,
                "credential mutation request is required",
                None,
                None,
            )),
        }
    }

    async fn create(
        &self,
        binding_id: String,
        reference: CredentialSecretReferenceDraft,
    ) -> RegistryAdminResponse {
        match self
            .registry
            .create_credential_binding(&binding_id, secret_reference(reference))
            .await
        {
            Ok(revision) => success(RegistryAdminResult::CredentialBindingCreated {
                generation: redact_stored_credential(&revision),
            }),
            Err(source) => failure(map_credential_error(source, &binding_id, Some(1))),
        }
    }

    async fn stage(
        &self,
        binding_id: String,
        generation: u64,
        expected_active: u64,
        reference: CredentialSecretReferenceDraft,
    ) -> RegistryAdminResponse {
        let definition = crate::registry_domain::CredentialBindingRevision {
            binding_id: binding_id.clone(),
            generation,
            reference: secret_reference(reference),
        };
        match self
            .registry
            .stage_credential(expected_active, definition)
            .await
        {
            Ok(revision) => success(RegistryAdminResult::CredentialGenerationStaged {
                generation: redact_stored_credential(&revision),
            }),
            Err(source) => failure(map_credential_error(source, &binding_id, Some(generation))),
        }
    }

    async fn activate(
        &self,
        binding_id: String,
        generation: u64,
        expected_active: u64,
    ) -> RegistryAdminResponse {
        if let Err(source) = self
            .registry
            .preflight_credential_generation(&binding_id, generation, self.resolver)
            .await
        {
            return failure(map_credential_error(source, &binding_id, Some(generation)));
        }
        match self
            .registry
            .activate_credential(&binding_id, generation, expected_active)
            .await
        {
            Ok(()) => success(RegistryAdminResult::CredentialGenerationActivated {
                binding_id_sha256: sha256(&binding_id),
                active_generation: generation,
            }),
            Err(source) => failure(map_credential_error(source, &binding_id, Some(generation))),
        }
    }

    async fn rollback(
        &self,
        binding_id: String,
        target: u64,
        expected_active: u64,
    ) -> RegistryAdminResponse {
        if let Err(source) = self
            .registry
            .preflight_credential_generation(&binding_id, target, self.resolver)
            .await
        {
            return failure(map_credential_error(source, &binding_id, Some(target)));
        }
        match self
            .registry
            .rollback_credential(&binding_id, target, expected_active)
            .await
        {
            Ok(()) => success(RegistryAdminResult::CredentialGenerationRolledBack {
                binding_id_sha256: sha256(&binding_id),
                active_generation: target,
            }),
            Err(source) => failure(map_credential_error(source, &binding_id, Some(target))),
        }
    }
}

fn secret_reference(reference: CredentialSecretReferenceDraft) -> SecretRef {
    match reference {
        CredentialSecretReferenceDraft::Environment { name } => SecretRef::Env { name },
        CredentialSecretReferenceDraft::File { path } => SecretRef::File { path: path.into() },
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

fn redact_model_revision(
    revision: &StoredRevision<ModelDefinition>,
) -> Result<ModelRevisionView, AgentRegistryError> {
    let pricing_sha256 = revision
        .definition
        .pricing
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(AgentRegistryError::serde)?
        .as_deref()
        .map(sha256);
    Ok(ModelRevisionView {
        definition: RedactedModelDefinition {
            provider_id: revision.definition.provider_id.clone(),
            model_id: revision.definition.model_id.clone(),
            revision: revision.definition.revision,
            context_window: revision.definition.context_window,
            max_output_tokens: revision.definition.max_output_tokens,
            capabilities: revision.definition.capabilities.iter().cloned().collect(),
            lifecycle: revision.definition.lifecycle.clone(),
            pricing_sha256,
        },
        digest_sha256: revision.digest.clone(),
        created_at_unix_secs: revision.created_at,
        active: revision.active,
    })
}

fn redact_credential_generation(view: &CredentialBindingView) -> CredentialGenerationView {
    CredentialGenerationView {
        binding_id_sha256: sha256(&view.binding_id),
        generation: view.generation,
        reference_kind: match view.reference_kind {
            SecretReferenceKind::Environment => CredentialReferenceKind::Environment,
            SecretReferenceKind::File => CredentialReferenceKind::File,
        },
        reference_configured: view.reference_configured,
        reference_digest_sha256: view.reference_digest_sha256.clone(),
        created_at_unix_secs: view.created_at,
        active: view.active,
    }
}

fn redact_stored_credential(
    revision: &StoredRevision<crate::registry_domain::CredentialBindingRevision>,
) -> CredentialGenerationView {
    CredentialGenerationView {
        binding_id_sha256: sha256(&revision.definition.binding_id),
        generation: revision.definition.generation,
        reference_kind: match revision.definition.reference {
            SecretRef::Env { .. } => CredentialReferenceKind::Environment,
            SecretRef::File { .. } => CredentialReferenceKind::File,
        },
        reference_configured: true,
        reference_digest_sha256: revision.digest.clone(),
        created_at_unix_secs: revision.created_at,
        active: revision.active,
    }
}

fn map_credential_error(
    source: CredentialRegistryError,
    binding_id: &str,
    generation: Option<u64>,
) -> RegistryAdminError {
    match source {
        CredentialRegistryError::UnknownBinding(_) => credential_error(
            RegistryAdminErrorCode::UnknownCredentialBinding,
            "credential binding is unknown",
            binding_id,
            None,
        ),
        CredentialRegistryError::UnknownGeneration { generation, .. } => credential_error(
            RegistryAdminErrorCode::UnknownGeneration,
            "credential generation is unknown",
            binding_id,
            Some(generation),
        ),
        CredentialRegistryError::AlreadyExists { .. } => credential_error(
            RegistryAdminErrorCode::CredentialAlreadyExists,
            "credential binding already exists",
            binding_id,
            generation,
        ),
        CredentialRegistryError::Conflict {
            expected, actual, ..
        } => credential_error_details(
            RegistryAdminErrorCode::ActiveGenerationConflict,
            "credential active generation changed",
            binding_id,
            generation,
            RegistryAdminErrorDetails::ActiveGenerationConflict {
                expected_active_generation: expected,
                actual_active_generation: actual,
            },
        ),
        CredentialRegistryError::NonSequential {
            expected, actual, ..
        } => credential_error_details(
            RegistryAdminErrorCode::NonSequentialGeneration,
            "credential generation is not sequential",
            binding_id,
            Some(actual),
            RegistryAdminErrorDetails::NonSequentialGeneration {
                expected_generation: expected,
                actual_generation: actual,
            },
        ),
        CredentialRegistryError::GenerationCollision { generation, .. } => {
            credential_error_details(
                RegistryAdminErrorCode::GenerationCollision,
                "credential generation has different content",
                binding_id,
                Some(generation),
                RegistryAdminErrorDetails::GenerationCollision { generation },
            )
        }
        CredentialRegistryError::InvalidRollback { target, actual } => credential_error_details(
            RegistryAdminErrorCode::InvalidRollback,
            "credential rollback target is invalid",
            binding_id,
            Some(target),
            RegistryAdminErrorDetails::InvalidRollback {
                target_generation: target,
                actual_active_generation: actual,
            },
        ),
        CredentialRegistryError::Resolution { generation, .. } => credential_error(
            RegistryAdminErrorCode::CredentialUnavailable,
            "credential generation is unavailable",
            binding_id,
            Some(generation),
        ),
        CredentialRegistryError::Registry(source) => {
            map_credential_storage_error(source, binding_id, generation)
        }
    }
}

fn map_credential_storage_error(
    source: AgentRegistryError,
    binding_id: &str,
    generation: Option<u64>,
) -> RegistryAdminError {
    let (code, message) = match source {
        AgentRegistryError::Storage(_) | AgentRegistryError::Task(_) => (
            RegistryAdminErrorCode::StorageUnavailable,
            "credential registry is unavailable",
        ),
        AgentRegistryError::Serialization(_) | AgentRegistryError::Integrity(_) => (
            RegistryAdminErrorCode::IntegrityFailure,
            "credential registry integrity check failed",
        ),
        AgentRegistryError::Invalid(_) => (
            RegistryAdminErrorCode::InvalidRequest,
            "credential registry request is invalid",
        ),
        _ => (
            RegistryAdminErrorCode::Internal,
            "credential registry operation failed",
        ),
    };
    credential_error(code, message, binding_id, generation)
}

fn credential_error(
    code: RegistryAdminErrorCode,
    message: &'static str,
    binding_id: &str,
    generation: Option<u64>,
) -> RegistryAdminError {
    RegistryAdminError {
        code,
        message: message.into(),
        provider_id: None,
        model_id: None,
        binding_id_sha256: Some(sha256(binding_id).into_boxed_str()),
        revision: None,
        generation,
        details: None,
    }
}

fn credential_error_details(
    code: RegistryAdminErrorCode,
    message: &'static str,
    binding_id: &str,
    generation: Option<u64>,
    details: RegistryAdminErrorDetails,
) -> RegistryAdminError {
    RegistryAdminError {
        details: Some(Box::new(details)),
        ..credential_error(code, message, binding_id, generation)
    }
}

fn map_model_error(
    source: ModelRegistryError,
    provider_id: String,
    model_id: String,
) -> RegistryAdminError {
    match source {
        ModelRegistryError::UnknownProvider(_) => model_error(
            RegistryAdminErrorCode::UnknownProvider,
            "provider is unknown",
            provider_id,
            model_id,
            None,
        ),
        ModelRegistryError::UnknownModel(_) => model_error(
            RegistryAdminErrorCode::UnknownModel,
            "model is unknown",
            provider_id,
            model_id,
            None,
        ),
        ModelRegistryError::UnknownRevision { revision, .. } => model_error(
            RegistryAdminErrorCode::UnknownRevision,
            "model revision is unknown",
            provider_id,
            model_id,
            Some(revision),
        ),
        ModelRegistryError::Registry(source) => {
            map_model_storage_error(source, provider_id, model_id, None)
        }
        ModelRegistryError::InvalidDefinition => model_error(
            RegistryAdminErrorCode::InvalidRequest,
            "model revision is invalid",
            provider_id,
            model_id,
            None,
        ),
        _ => model_error(
            RegistryAdminErrorCode::Internal,
            "model registry operation failed",
            provider_id,
            model_id,
            None,
        ),
    }
}

fn map_model_storage_error(
    source: AgentRegistryError,
    provider_id: String,
    model_id: String,
    revision: Option<u64>,
) -> RegistryAdminError {
    let (code, message) = match source {
        AgentRegistryError::Storage(_) | AgentRegistryError::Task(_) => (
            RegistryAdminErrorCode::StorageUnavailable,
            "model registry is unavailable",
        ),
        AgentRegistryError::Serialization(_) | AgentRegistryError::Integrity(_) => (
            RegistryAdminErrorCode::IntegrityFailure,
            "model registry integrity check failed",
        ),
        AgentRegistryError::Invalid(_) => (
            RegistryAdminErrorCode::InvalidRequest,
            "model revision is invalid",
        ),
        _ => (
            RegistryAdminErrorCode::Internal,
            "model registry operation failed",
        ),
    };
    model_error(code, message, provider_id, model_id, revision)
}

fn model_error(
    code: RegistryAdminErrorCode,
    message: &'static str,
    provider_id: String,
    model_id: String,
    revision: Option<u64>,
) -> RegistryAdminError {
    RegistryAdminError {
        code,
        message: message.into(),
        provider_id: Some(provider_id.into_boxed_str()),
        model_id: Some(model_id.into_boxed_str()),
        binding_id_sha256: None,
        revision,
        generation: None,
        details: None,
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
        provider_id: provider_id.map(String::into_boxed_str),
        model_id: None,
        binding_id_sha256: None,
        revision,
        generation: None,
        details: None,
    }
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use sylvander_protocol::{
        AuthenticationMethod, ModelLifecycle, ModelPricing, RegistryAdminErrorCode,
    };

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

    fn model(revision: u64) -> ModelDefinition {
        ModelDefinition {
            provider_id: "alpha".into(),
            model_id: "shared".into(),
            revision,
            context_window: 100_000 + u32::try_from(revision).unwrap(),
            max_output_tokens: 4096,
            capabilities: BTreeSet::from(["tool_use".into()]),
            lifecycle: ModelLifecycle::Active,
            pricing: Some(ModelPricing {
                input_usd_micros_per_million: revision * 100,
                output_usd_micros_per_million: revision * 200,
                cache_write_usd_micros_per_million: None,
                cache_read_usd_micros_per_million: None,
            }),
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

    #[tokio::test]
    async fn model_reads_are_pinned_paginated_and_pricing_redacted() {
        let registry = registry().await;
        registry.seed_model(model(1)).await.unwrap();
        registry.stage_model(1, model(2)).await.unwrap();
        registry.stage_model(1, model(3)).await.unwrap();
        registry
            .activate_model(("alpha", "shared"), 3, 1)
            .await
            .unwrap();
        let service = RegistryAdminService::new(&registry);

        let inspected = service
            .dispatch(
                Some(&admin()),
                RegistryAdminRequest::InspectModelRevision {
                    provider_id: "alpha".into(),
                    model_id: "shared".into(),
                    revision: 1,
                },
            )
            .await;
        let encoded = serde_json::to_string(&inspected).unwrap();
        for raw_price in [
            "input_usd_micros_per_million",
            "output_usd_micros_per_million",
        ] {
            assert!(!encoded.contains(raw_price));
        }
        let RegistryAdminResponse::Success { result } = inspected else {
            panic!("expected model inspection");
        };
        let RegistryAdminResult::ModelRevisionInspected { revision } = *result else {
            panic!("expected model revision");
        };
        assert_eq!(revision.definition.revision, 1);
        assert!(!revision.active);
        assert!(revision.definition.pricing_sha256.is_some());

        let first = service
            .dispatch(
                Some(&admin()),
                RegistryAdminRequest::ListModelRevisions {
                    provider_id: "alpha".into(),
                    model_id: "shared".into(),
                    before_revision: None,
                    limit: 2,
                },
            )
            .await;
        let RegistryAdminResponse::Success { result } = first else {
            panic!("expected model list");
        };
        let RegistryAdminResult::ModelRevisionsListed {
            active_revision,
            revisions,
            next_before_revision,
            ..
        } = *result
        else {
            panic!("expected model revisions");
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

        let unknown = service
            .dispatch(
                Some(&admin()),
                RegistryAdminRequest::ListModelRevisions {
                    provider_id: "alpha".into(),
                    model_id: "missing".into(),
                    before_revision: None,
                    limit: 1,
                },
            )
            .await;
        assert!(matches!(
            unknown,
            RegistryAdminResponse::Error { error }
                if error.code == RegistryAdminErrorCode::UnknownModel
                    && error.provider_id.as_deref() == Some("alpha")
                    && error.model_id.as_deref() == Some("missing")
        ));
    }

    #[tokio::test]
    async fn credential_reads_are_bounded_hashed_and_never_resolve_secrets() {
        let registry = registry().await;
        for generation in 2..=3 {
            registry
                .stage_credential(
                    1,
                    CredentialBindingRevision {
                        binding_id: RAW_BINDING.into(),
                        generation,
                        reference: SecretRef::File {
                            path: format!("/private/credential-{generation}").into(),
                        },
                    },
                )
                .await
                .unwrap();
        }
        registry
            .activate_credential(RAW_BINDING, 3, 1)
            .await
            .unwrap();
        let service = RegistryAdminService::new(&registry);

        let inspected = service
            .dispatch(
                Some(&admin()),
                RegistryAdminRequest::InspectCredentialGeneration {
                    binding_id: RAW_BINDING.into(),
                    generation: 1,
                },
            )
            .await;
        let encoded = serde_json::to_string(&inspected).unwrap();
        let debug = format!("{inspected:?}");
        for marker in [
            RAW_BINDING,
            "UNRESOLVED_TEST_REFERENCE",
            "/private/credential-2",
        ] {
            assert!(!encoded.contains(marker));
            assert!(!debug.contains(marker));
        }
        let RegistryAdminResponse::Success { result } = inspected else {
            panic!("expected credential inspection");
        };
        let RegistryAdminResult::CredentialGenerationInspected { generation } = *result else {
            panic!("expected credential generation");
        };
        assert_eq!(generation.binding_id_sha256, sha256(RAW_BINDING));
        assert_eq!(generation.generation, 1);
        assert!(!generation.active);
        assert_eq!(
            generation.reference_kind,
            CredentialReferenceKind::Environment
        );

        let listed = service
            .dispatch(
                Some(&admin()),
                RegistryAdminRequest::ListCredentialGenerations {
                    binding_id: RAW_BINDING.into(),
                    before_generation: None,
                    limit: 2,
                },
            )
            .await;
        let RegistryAdminResponse::Success { result } = listed else {
            panic!("expected credential list");
        };
        let RegistryAdminResult::CredentialGenerationsListed {
            binding_id_sha256,
            active_generation,
            generations,
            next_before_generation,
        } = *result
        else {
            panic!("expected credential generations");
        };
        assert_eq!(binding_id_sha256, sha256(RAW_BINDING));
        assert_eq!(active_generation, 3);
        assert_eq!(
            generations
                .iter()
                .map(|item| item.generation)
                .collect::<Vec<_>>(),
            [3, 2]
        );
        assert_eq!(next_before_generation, Some(2));

        let unknown = service
            .dispatch(
                Some(&admin()),
                RegistryAdminRequest::InspectCredentialGeneration {
                    binding_id: RAW_BINDING.into(),
                    generation: 99,
                },
            )
            .await;
        assert!(matches!(
            unknown,
            RegistryAdminResponse::Error { error }
                if error.code == RegistryAdminErrorCode::UnknownGeneration
                    && error.binding_id_sha256.as_deref() == Some(sha256(RAW_BINDING).as_str())
                    && error.generation == Some(99)
        ));
    }
}
