//! Resolve immutable registry bindings without consulting Provider or Model heads.

use sylvander_protocol::AgentId;

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::agent_registry_snapshot::{AgentSnapshotError, SnapshotModel};
use crate::config::AgentDefinitionConfig;
use crate::credential_registry::CredentialRegistryError;
use crate::registry_domain::{ModelDefinition, ProviderDefinition};

#[derive(Debug, Clone)]
pub(crate) struct RegistryCompositionSnapshot {
    pub agent: AgentDefinitionConfig,
    pub provider: ProviderDefinition,
    pub models: Vec<ModelDefinition>,
    pub default_model_id: String,
    pub credential_binding_id: String,
}

impl AgentRegistry {
    pub(crate) async fn resolve_registry_composition(
        &self,
        agent_id: &AgentId,
        revision: u64,
    ) -> Result<RegistryCompositionSnapshot, RegistryCompositionError> {
        let agent = self
            .load(agent_id, revision)
            .await?
            .ok_or_else(|| RegistryCompositionError::UnknownAgentRevision {
                agent_id: agent_id.to_string(),
                revision,
            })?
            .definition;
        let snapshot = self
            .load_agent_snapshot(&agent_id.0, revision)
            .await?
            .ok_or_else(|| RegistryCompositionError::MissingSnapshot {
                agent_id: agent_id.to_string(),
                revision,
            })?;
        snapshot.validate()?;
        if snapshot.agent_id != agent_id.0 || snapshot.agent_revision != revision {
            return Err(RegistryCompositionError::AgentMismatch);
        }
        let provider = self
            .load_provider_revision(&snapshot.provider_id, snapshot.provider_revision)
            .await?
            .ok_or_else(|| RegistryCompositionError::UnknownProviderRevision {
                provider_id: snapshot.provider_id.clone(),
                revision: snapshot.provider_revision,
            })?
            .definition;
        if provider.id != snapshot.provider_id
            || provider.revision != snapshot.provider_revision
            || agent.spec.model.provider != provider.id
        {
            return Err(RegistryCompositionError::ProviderMismatch);
        }

        let credential_binding_id = provider.credential_binding_id.clone();
        let credential_exists = self
            .inspect_credentials(&credential_binding_id)
            .await?
            .into_iter()
            .any(|revision| revision.active);
        if !credential_exists {
            return Err(RegistryCompositionError::MissingCredentialBinding(
                credential_binding_id,
            ));
        }

        let mut models = Vec::with_capacity(snapshot.models.len());
        let mut default_model_id = None;
        for binding in &snapshot.models {
            validate_binding(&provider, binding)?;
            let model = self
                .load_model_revision(&binding.provider_id, &binding.model_id, binding.revision)
                .await?
                .ok_or_else(|| RegistryCompositionError::UnknownModelRevision {
                    provider_id: binding.provider_id.clone(),
                    model_id: binding.model_id.clone(),
                    revision: binding.revision,
                })?
                .definition;
            if model.provider_id != binding.provider_id
                || model.model_id != binding.model_id
                || model.revision != binding.revision
            {
                return Err(RegistryCompositionError::ModelMismatch);
            }
            if binding.is_default {
                default_model_id = Some(binding.model_id.clone());
            }
            models.push(model);
        }
        let default_model_id = default_model_id.ok_or(RegistryCompositionError::MissingDefault)?;
        if agent.spec.model.model_name != default_model_id {
            return Err(RegistryCompositionError::DefaultModelMismatch {
                configured: agent.spec.model.model_name,
                snapshot: default_model_id,
            });
        }
        Ok(RegistryCompositionSnapshot {
            agent,
            provider,
            models,
            default_model_id,
            credential_binding_id,
        })
    }
}

fn validate_binding(
    provider: &ProviderDefinition,
    binding: &SnapshotModel,
) -> Result<(), RegistryCompositionError> {
    if binding.provider_id == provider.id {
        Ok(())
    } else {
        Err(RegistryCompositionError::ModelProviderMismatch {
            provider_id: provider.id.clone(),
            model_id: binding.model_id.clone(),
            model_provider_id: binding.provider_id.clone(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RegistryCompositionError {
    #[error("unknown Agent revision `{agent_id}`@{revision}")]
    UnknownAgentRevision { agent_id: String, revision: u64 },
    #[error("Agent revision `{agent_id}`@{revision} has no immutable registry snapshot")]
    MissingSnapshot { agent_id: String, revision: u64 },
    #[error("registry snapshot does not match the requested Agent revision")]
    AgentMismatch,
    #[error("unknown Provider revision `{provider_id}`@{revision}")]
    UnknownProviderRevision { provider_id: String, revision: u64 },
    #[error("registry snapshot Provider does not match the Agent definition")]
    ProviderMismatch,
    #[error("credential binding `{0}` has no active generation")]
    MissingCredentialBinding(String),
    #[error("unknown Model revision `{provider_id}/{model_id}`@{revision}")]
    UnknownModelRevision {
        provider_id: String,
        model_id: String,
        revision: u64,
    },
    #[error("Model `{model_id}` belongs to Provider `{model_provider_id}`, not `{provider_id}`")]
    ModelProviderMismatch {
        provider_id: String,
        model_id: String,
        model_provider_id: String,
    },
    #[error("stored Model definition does not match its immutable binding")]
    ModelMismatch,
    #[error("registry snapshot has no default Model")]
    MissingDefault,
    #[error("Agent default Model `{configured}` does not match snapshot `{snapshot}`")]
    DefaultModelMismatch {
        configured: String,
        snapshot: String,
    },
    #[error(transparent)]
    Snapshot(#[from] AgentSnapshotError),
    #[error(transparent)]
    Credential(#[from] CredentialRegistryError),
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
}
