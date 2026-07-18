//! Privileged administration of immutable runtime registries.

use sha2::{Digest, Sha256};
use sylvander_protocol::{
    AuthenticatedPrincipal, CredentialGenerationView, CredentialReferenceKind,
    CredentialSecretReferenceDraft, ModelDefinitionDraft, ModelLifecycle, ModelLifecycleDraft,
    ModelPricing, ModelPricingDraft, ModelRevisionView, ProviderDefinitionDraft,
    ProviderRevisionView, RedactedModelDefinition, RedactedProviderDefinition, RegistryAdminError,
    RegistryAdminErrorCode, RegistryAdminErrorDetails, RegistryAdminRequest, RegistryAdminResponse,
    RegistryAdminResult,
};

use crate::agent_admin::is_agent_administrator;
use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::config::SecretRef;
use crate::credential_audit::{
    CredentialAuditOperation, CredentialAuditResult, CredentialAuditSubject,
    CredentialOperationAuditLedger,
};
use crate::credential_registry::{CredentialRegistryError, CredentialSecretResolver};
use crate::model_registry::ModelRegistryError;
use crate::provider_registry::ProviderRegistryError;
use crate::registry_domain::{
    CredentialBindingView, ModelDefinition, ProviderDefinition, SecretReferenceKind,
    StoredRevision, canonicalize_model_capabilities,
};
use crate::request_scoped_provider::AnthropicProviderFactory;

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
            RegistryAdminRequest::CreateProvider {
                provider_id,
                definition,
            } => self.create_provider(provider_id, definition).await,
            RegistryAdminRequest::StageProviderRevision {
                provider_id,
                revision,
                expected_active_revision,
                definition,
            } => {
                self.stage_provider(provider_id, revision, expected_active_revision, definition)
                    .await
            }
            RegistryAdminRequest::ActivateProviderRevision {
                provider_id,
                revision,
                expected_active_revision,
            } => {
                self.activate_provider(provider_id, revision, expected_active_revision)
                    .await
            }
            RegistryAdminRequest::RollbackProviderRevision {
                provider_id,
                target_revision,
                expected_active_revision,
            } => {
                self.rollback_provider(provider_id, target_revision, expected_active_revision)
                    .await
            }
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
            RegistryAdminRequest::CreateModel {
                provider_id,
                model_id,
                definition,
            } => self.create_model(provider_id, model_id, definition).await,
            RegistryAdminRequest::StageModelRevision {
                provider_id,
                model_id,
                revision,
                expected_active_revision,
                definition,
            } => {
                self.stage_model(
                    provider_id,
                    model_id,
                    revision,
                    expected_active_revision,
                    definition,
                )
                .await
            }
            RegistryAdminRequest::ActivateModelRevision {
                provider_id,
                model_id,
                revision,
                expected_active_revision,
            } => {
                self.activate_model(provider_id, model_id, revision, expected_active_revision)
                    .await
            }
            RegistryAdminRequest::RollbackModelRevision {
                provider_id,
                model_id,
                target_revision,
                expected_active_revision,
            } => {
                self.rollback_model(
                    provider_id,
                    model_id,
                    target_revision,
                    expected_active_revision,
                )
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

    async fn create_provider(
        &self,
        provider_id: String,
        definition: ProviderDefinitionDraft,
    ) -> RegistryAdminResponse {
        let definition = provider_definition(provider_id.clone(), 1, definition);
        if AnthropicProviderFactory::validate_definition(&definition).is_err() {
            return invalid_provider_definition(provider_id, Some(1));
        }
        match self.registry.create_provider(definition).await {
            Ok(stored) => success(RegistryAdminResult::ProviderCreated {
                revision: redact_provider_revision(&stored),
            }),
            Err(source) => failure(map_provider_error(source, provider_id, Some(1))),
        }
    }

    async fn stage_provider(
        &self,
        provider_id: String,
        revision: u64,
        expected_active: u64,
        definition: ProviderDefinitionDraft,
    ) -> RegistryAdminResponse {
        let definition = provider_definition(provider_id.clone(), revision, definition);
        if AnthropicProviderFactory::validate_definition(&definition).is_err() {
            return invalid_provider_definition(provider_id, Some(revision));
        }
        match self
            .registry
            .stage_provider(expected_active, definition)
            .await
        {
            Ok(stored) => success(RegistryAdminResult::ProviderRevisionStaged {
                revision: redact_provider_revision(&stored),
            }),
            Err(source) => failure(map_provider_error(source, provider_id, Some(revision))),
        }
    }

    async fn activate_provider(
        &self,
        provider_id: String,
        revision: u64,
        expected_active: u64,
    ) -> RegistryAdminResponse {
        if let Err(response) = self.preflight_provider(&provider_id, revision).await {
            return response;
        }
        match self
            .registry
            .activate_provider(&provider_id, revision, expected_active)
            .await
        {
            Ok(stored) => success(RegistryAdminResult::ProviderRevisionActivated {
                revision: redact_provider_revision(&stored),
            }),
            Err(source) => failure(map_provider_error(source, provider_id, Some(revision))),
        }
    }

    async fn rollback_provider(
        &self,
        provider_id: String,
        target: u64,
        expected_active: u64,
    ) -> RegistryAdminResponse {
        if let Err(response) = self.preflight_provider(&provider_id, target).await {
            return response;
        }
        match self
            .registry
            .rollback_provider(&provider_id, target, expected_active)
            .await
        {
            Ok(stored) => success(RegistryAdminResult::ProviderRevisionRolledBack {
                revision: redact_provider_revision(&stored),
            }),
            Err(source) => failure(map_provider_error(source, provider_id, Some(target))),
        }
    }

    async fn preflight_provider(
        &self,
        provider_id: &str,
        revision: u64,
    ) -> Result<(), RegistryAdminResponse> {
        let stored = match self
            .registry
            .load_provider_revision(provider_id, revision)
            .await
        {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                return Err(failure(error(
                    RegistryAdminErrorCode::UnknownRevision,
                    "provider revision is unknown",
                    Some(provider_id.to_owned()),
                    Some(revision),
                )));
            }
            Err(source) => {
                return Err(failure(map_registry_error(
                    source,
                    provider_id.to_owned(),
                    Some(revision),
                )));
            }
        };
        AnthropicProviderFactory::validate_definition(&stored.definition)
            .map_err(|_| invalid_provider_definition(provider_id.to_owned(), Some(revision)))
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
            Err(source) => failure(map_provider_error(source, provider_id, None)),
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
            Err(source) => failure(map_model_error(source, provider_id, model_id, None)),
        }
    }

    async fn create_model(
        &self,
        provider_id: String,
        model_id: String,
        draft: ModelDefinitionDraft,
    ) -> RegistryAdminResponse {
        let definition = match model_definition(provider_id.clone(), model_id.clone(), 1, draft) {
            Ok(definition) => definition,
            Err(error) => return failure(error),
        };
        match self.registry.create_model(definition).await {
            Ok(stored) => model_revision_response(&stored, provider_id, model_id, |revision| {
                RegistryAdminResult::ModelCreated { revision }
            }),
            Err(source) => failure(map_model_error(source, provider_id, model_id, Some(1))),
        }
    }

    async fn stage_model(
        &self,
        provider_id: String,
        model_id: String,
        revision: u64,
        expected_active: u64,
        draft: ModelDefinitionDraft,
    ) -> RegistryAdminResponse {
        let definition =
            match model_definition(provider_id.clone(), model_id.clone(), revision, draft) {
                Ok(definition) => definition,
                Err(error) => return failure(error),
            };
        match self.registry.stage_model(expected_active, definition).await {
            Ok(stored) => model_revision_response(&stored, provider_id, model_id, |revision| {
                RegistryAdminResult::ModelRevisionStaged { revision }
            }),
            Err(source) => failure(map_model_error(
                source,
                provider_id,
                model_id,
                Some(revision),
            )),
        }
    }

    async fn activate_model(
        &self,
        provider_id: String,
        model_id: String,
        revision: u64,
        expected_active: u64,
    ) -> RegistryAdminResponse {
        match self
            .registry
            .activate_model((&provider_id, &model_id), revision, expected_active)
            .await
        {
            Ok(stored) => model_revision_response(&stored, provider_id, model_id, |revision| {
                RegistryAdminResult::ModelRevisionActivated { revision }
            }),
            Err(source) => failure(map_model_error(
                source,
                provider_id,
                model_id,
                Some(revision),
            )),
        }
    }

    async fn rollback_model(
        &self,
        provider_id: String,
        model_id: String,
        target: u64,
        expected_active: u64,
    ) -> RegistryAdminResponse {
        match self
            .registry
            .rollback_model((&provider_id, &model_id), target, expected_active)
            .await
        {
            Ok(stored) => model_revision_response(&stored, provider_id, model_id, |revision| {
                RegistryAdminResult::ModelRevisionRolledBack { revision }
            }),
            Err(source) => failure(map_model_error(source, provider_id, model_id, Some(target))),
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
    audit: &'a CredentialOperationAuditLedger,
}

impl<'a> CredentialRegistryMutationService<'a> {
    #[must_use]
    pub(crate) const fn new(
        registry: &'a AgentRegistry,
        resolver: &'a dyn CredentialSecretResolver,
        audit: &'a CredentialOperationAuditLedger,
    ) -> Self {
        Self {
            registry,
            resolver,
            audit,
        }
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
        let audit_target = credential_mutation_audit_target(&request);
        let response = match request {
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
        };
        let (subject, operation, revision) = audit_target;
        let (operation, result) = credential_mutation_audit_outcome(&response, operation);
        if self
            .audit
            .record(&subject, operation, revision, result)
            .await
            .is_err()
        {
            return failure(error(
                RegistryAdminErrorCode::StorageUnavailable,
                "credential operation audit is unavailable",
                None,
                revision,
            ));
        }
        response
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

fn credential_mutation_audit_target(
    request: &RegistryAdminRequest,
) -> (
    CredentialAuditSubject,
    CredentialAuditOperation,
    Option<u64>,
) {
    let (binding_id, operation, revision) = match request {
        RegistryAdminRequest::CreateCredentialBinding { binding_id, .. } => {
            (binding_id, CredentialAuditOperation::Create, Some(1))
        }
        RegistryAdminRequest::StageCredentialGeneration {
            binding_id,
            generation,
            ..
        }
        | RegistryAdminRequest::ActivateCredentialGeneration {
            binding_id,
            generation,
            ..
        } => (
            binding_id,
            CredentialAuditOperation::Rotate,
            Some(*generation),
        ),
        RegistryAdminRequest::RollbackCredentialGeneration {
            binding_id,
            expected_active_generation,
            ..
        } => (
            binding_id,
            CredentialAuditOperation::Revoke,
            Some(*expected_active_generation),
        ),
        _ => unreachable!("validated credential mutation dispatch target"),
    };
    (
        CredentialAuditSubject::provider_binding(binding_id)
            .expect("validated registry binding identity"),
        operation,
        revision,
    )
}

fn credential_mutation_audit_outcome(
    response: &RegistryAdminResponse,
    success_operation: CredentialAuditOperation,
) -> (CredentialAuditOperation, CredentialAuditResult) {
    match response {
        RegistryAdminResponse::Success { .. } => {
            (success_operation, CredentialAuditResult::Succeeded)
        }
        RegistryAdminResponse::Error { error } => (
            CredentialAuditOperation::Failure,
            match error.code {
                RegistryAdminErrorCode::InvalidRequest
                | RegistryAdminErrorCode::UnknownCredentialBinding
                | RegistryAdminErrorCode::UnknownGeneration
                | RegistryAdminErrorCode::CredentialAlreadyExists
                | RegistryAdminErrorCode::NonSequentialGeneration
                | RegistryAdminErrorCode::GenerationCollision
                | RegistryAdminErrorCode::InvalidRollback
                | RegistryAdminErrorCode::Unauthorized
                | RegistryAdminErrorCode::UnknownProvider
                | RegistryAdminErrorCode::UnknownModel
                | RegistryAdminErrorCode::UnknownRevision
                | RegistryAdminErrorCode::ProviderAlreadyExists
                | RegistryAdminErrorCode::ModelAlreadyExists
                | RegistryAdminErrorCode::NonSequentialRevision
                | RegistryAdminErrorCode::RevisionCollision
                | RegistryAdminErrorCode::InvalidRevisionRollback
                | RegistryAdminErrorCode::Internal => CredentialAuditResult::InvalidRequest,
                RegistryAdminErrorCode::ActiveGenerationConflict
                | RegistryAdminErrorCode::ActiveRevisionConflict => CredentialAuditResult::Conflict,
                RegistryAdminErrorCode::CredentialUnavailable => CredentialAuditResult::Unavailable,
                RegistryAdminErrorCode::StorageUnavailable => {
                    CredentialAuditResult::StorageUnavailable
                }
                RegistryAdminErrorCode::IntegrityFailure => CredentialAuditResult::Integrity,
            },
        ),
    }
}

fn secret_reference(reference: CredentialSecretReferenceDraft) -> SecretRef {
    match reference {
        CredentialSecretReferenceDraft::Environment { name } => SecretRef::Env { name },
        CredentialSecretReferenceDraft::File { path } => SecretRef::File { path: path.into() },
    }
}

fn provider_definition(
    provider_id: String,
    revision: u64,
    draft: ProviderDefinitionDraft,
) -> ProviderDefinition {
    ProviderDefinition {
        id: provider_id,
        revision,
        kind: draft.kind,
        base_url: draft.base_url,
        credential_binding_id: draft.credential_binding_id,
    }
}

fn model_definition(
    provider_id: String,
    model_id: String,
    revision: u64,
    draft: ModelDefinitionDraft,
) -> Result<ModelDefinition, RegistryAdminError> {
    let capabilities = canonicalize_model_capabilities(&draft.capabilities).map_err(|_| {
        model_error(
            RegistryAdminErrorCode::InvalidRequest,
            "model revision is invalid",
            provider_id.clone(),
            model_id.clone(),
            Some(revision),
        )
    })?;
    Ok(ModelDefinition {
        provider_id,
        model_id,
        revision,
        context_window: draft.context_window,
        max_output_tokens: draft.max_output_tokens,
        capabilities,
        lifecycle: match draft.lifecycle {
            ModelLifecycleDraft::Active {} => ModelLifecycle::Active,
            ModelLifecycleDraft::Deprecated { replacement } => {
                ModelLifecycle::Deprecated { replacement }
            }
        },
        pricing: draft.pricing.map(model_pricing),
    })
}

fn model_pricing(pricing: ModelPricingDraft) -> ModelPricing {
    ModelPricing {
        input_usd_micros_per_million: pricing.input_usd_micros_per_million,
        output_usd_micros_per_million: pricing.output_usd_micros_per_million,
        cache_write_usd_micros_per_million: pricing.cache_write_usd_micros_per_million,
        cache_read_usd_micros_per_million: pricing.cache_read_usd_micros_per_million,
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

fn model_revision_response(
    stored: &StoredRevision<ModelDefinition>,
    provider_id: String,
    model_id: String,
    result: impl FnOnce(ModelRevisionView) -> RegistryAdminResult,
) -> RegistryAdminResponse {
    match redact_model_revision(stored) {
        Ok(revision) => success(result(revision)),
        Err(source) => failure(map_model_storage_error(
            source,
            provider_id,
            model_id,
            Some(stored.definition.revision),
        )),
    }
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
    requested_revision: Option<u64>,
) -> RegistryAdminError {
    match source {
        ModelRegistryError::InvalidDefinition => model_error(
            RegistryAdminErrorCode::InvalidRequest,
            "model revision is invalid",
            provider_id,
            model_id,
            requested_revision,
        ),
        ModelRegistryError::IncompatibleProvider(_) => model_error(
            RegistryAdminErrorCode::InvalidRequest,
            "model revision is not supported by the active provider",
            provider_id,
            model_id,
            requested_revision,
        ),
        ModelRegistryError::AlreadyExists { .. } => model_error(
            RegistryAdminErrorCode::ModelAlreadyExists,
            "model already exists",
            provider_id,
            model_id,
            requested_revision,
        ),
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
        ModelRegistryError::Conflict {
            expected, actual, ..
        } => model_error_details(
            RegistryAdminErrorCode::ActiveRevisionConflict,
            "model active revision changed",
            provider_id,
            model_id,
            requested_revision,
            RegistryAdminErrorDetails::ActiveRevisionConflict {
                expected_active_revision: expected,
                actual_active_revision: actual,
            },
        ),
        ModelRegistryError::NonSequential {
            expected, actual, ..
        } => model_error_details(
            RegistryAdminErrorCode::NonSequentialRevision,
            "model revision is not sequential",
            provider_id,
            model_id,
            Some(actual),
            RegistryAdminErrorDetails::NonSequentialRevision {
                expected_revision: expected,
                actual_revision: actual,
            },
        ),
        ModelRegistryError::RevisionCollision { revision, .. } => model_error_details(
            RegistryAdminErrorCode::RevisionCollision,
            "model revision has different content",
            provider_id,
            model_id,
            Some(revision),
            RegistryAdminErrorDetails::RevisionCollision { revision },
        ),
        ModelRegistryError::InvalidRollback { target, actual } => model_error_details(
            RegistryAdminErrorCode::InvalidRevisionRollback,
            "model rollback target is invalid",
            provider_id,
            model_id,
            Some(target),
            RegistryAdminErrorDetails::InvalidRevisionRollback {
                target_revision: target,
                actual_active_revision: actual,
            },
        ),
        ModelRegistryError::Registry(source) => {
            map_model_storage_error(source, provider_id, model_id, requested_revision)
        }
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

fn model_error_details(
    code: RegistryAdminErrorCode,
    message: &'static str,
    provider_id: String,
    model_id: String,
    revision: Option<u64>,
    details: RegistryAdminErrorDetails,
) -> RegistryAdminError {
    RegistryAdminError {
        details: Some(Box::new(details)),
        ..model_error(code, message, provider_id, model_id, revision)
    }
}

fn map_provider_error(
    source: ProviderRegistryError,
    provider_id: String,
    requested_revision: Option<u64>,
) -> RegistryAdminError {
    match source {
        ProviderRegistryError::InvalidDefinition => error(
            RegistryAdminErrorCode::InvalidRequest,
            "provider revision is invalid",
            Some(provider_id),
            requested_revision,
        ),
        ProviderRegistryError::IncompatibleModel(_) => error(
            RegistryAdminErrorCode::InvalidRequest,
            "provider revision is not supported by its active models",
            Some(provider_id),
            requested_revision,
        ),
        ProviderRegistryError::AlreadyExists { .. } => error(
            RegistryAdminErrorCode::ProviderAlreadyExists,
            "provider already exists",
            Some(provider_id),
            requested_revision,
        ),
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
        ProviderRegistryError::Conflict {
            expected, actual, ..
        } => provider_error_details(
            RegistryAdminErrorCode::ActiveRevisionConflict,
            "provider active revision changed",
            provider_id,
            requested_revision,
            RegistryAdminErrorDetails::ActiveRevisionConflict {
                expected_active_revision: expected,
                actual_active_revision: actual,
            },
        ),
        ProviderRegistryError::NonSequential {
            expected, actual, ..
        } => provider_error_details(
            RegistryAdminErrorCode::NonSequentialRevision,
            "provider revision is not sequential",
            provider_id,
            Some(actual),
            RegistryAdminErrorDetails::NonSequentialRevision {
                expected_revision: expected,
                actual_revision: actual,
            },
        ),
        ProviderRegistryError::RevisionCollision { revision, .. } => provider_error_details(
            RegistryAdminErrorCode::RevisionCollision,
            "provider revision has different content",
            provider_id,
            Some(revision),
            RegistryAdminErrorDetails::RevisionCollision { revision },
        ),
        ProviderRegistryError::InvalidRollback { target, actual } => provider_error_details(
            RegistryAdminErrorCode::InvalidRevisionRollback,
            "provider rollback target is invalid",
            provider_id,
            Some(target),
            RegistryAdminErrorDetails::InvalidRevisionRollback {
                target_revision: target,
                actual_active_revision: actual,
            },
        ),
        ProviderRegistryError::Registry(source) => map_registry_error(source, provider_id, None),
    }
}

fn provider_error_details(
    code: RegistryAdminErrorCode,
    message: &'static str,
    provider_id: String,
    revision: Option<u64>,
    details: RegistryAdminErrorDetails,
) -> RegistryAdminError {
    RegistryAdminError {
        details: Some(Box::new(details)),
        ..error(code, message, Some(provider_id), revision)
    }
}

fn invalid_provider_definition(
    provider_id: String,
    revision: Option<u64>,
) -> RegistryAdminResponse {
    failure(error(
        RegistryAdminErrorCode::InvalidRequest,
        "provider revision is not supported",
        Some(provider_id),
        revision,
    ))
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
#[path = "../tests/unit/registry_admin.rs"]
mod tests;
