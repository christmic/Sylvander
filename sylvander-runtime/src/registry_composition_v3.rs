//! Resolve one immutable, multi-Provider registry composition.
//!
//! Resolution never consults Provider or Model heads. Native V3 and lifted
//! V2 snapshots both arrive through the versioned snapshot loader, after
//! which every component is loaded at its exact persisted revision.

use std::collections::BTreeMap;

use sylvander_protocol::{AgentId, ModelSelection};

use crate::agent_registry::{AgentRegistry, AgentRegistryError};
use crate::agent_registry_snapshot_v3::AgentSnapshotV3Error;
use crate::config::AgentDefinitionConfig;
use crate::credential_registry::CredentialRegistryError;
use crate::registry_domain::{ModelDefinition, ProviderDefinition};

/// Complete immutable registry closure for one Agent revision.
#[derive(Debug, Clone)]
pub(crate) struct VersionedRegistryCompositionSnapshot {
    pub agent: AgentDefinitionConfig,
    pub providers: BTreeMap<String, ProviderDefinition>,
    pub models: BTreeMap<ModelSelection, ModelDefinition>,
    pub default_model: ModelSelection,
}

impl AgentRegistry {
    /// Resolve an Agent and all of its version-pinned registry components.
    ///
    /// Missing or inconsistent state is an error. In particular, this never
    /// substitutes an active Provider or Model revision for a missing pin.
    pub(crate) async fn resolve_registry_composition_versioned(
        &self,
        agent_id: &AgentId,
        revision: u64,
    ) -> Result<VersionedRegistryCompositionSnapshot, VersionedRegistryCompositionError> {
        let agent = self
            .load(agent_id, revision)
            .await?
            .ok_or_else(|| VersionedRegistryCompositionError::UnknownAgentRevision {
                agent_id: agent_id.to_string(),
                revision,
            })?
            .definition;
        if agent.spec.id != *agent_id || agent.revision != revision {
            return Err(VersionedRegistryCompositionError::AgentDefinitionMismatch {
                agent_id: agent_id.to_string(),
                revision,
            });
        }

        let snapshot = self
            .load_agent_snapshot_versioned(&agent_id.0, revision)
            .await?
            .ok_or_else(|| VersionedRegistryCompositionError::MissingSnapshot {
                agent_id: agent_id.to_string(),
                revision,
            })?;
        if snapshot.agent_id != agent_id.0 || snapshot.agent_revision != revision {
            return Err(
                VersionedRegistryCompositionError::SnapshotIdentityMismatch {
                    agent_id: agent_id.to_string(),
                    revision,
                },
            );
        }

        let configured_default = ModelSelection {
            provider_id: agent.spec.model.provider.clone(),
            model_id: agent.spec.model.model_name.clone(),
        };
        if snapshot.default_model != configured_default {
            return Err(VersionedRegistryCompositionError::DefaultModelMismatch {
                configured: configured_default,
                snapshot: snapshot.default_model,
            });
        }

        let mut providers = BTreeMap::new();
        for (provider_id, provider_revision) in &snapshot.providers {
            let provider = self
                .load_provider_revision(provider_id, *provider_revision)
                .await?
                .ok_or_else(
                    || VersionedRegistryCompositionError::UnknownProviderRevision {
                        provider_id: provider_id.clone(),
                        revision: *provider_revision,
                    },
                )?
                .definition;
            if provider.id != *provider_id || provider.revision != *provider_revision {
                return Err(
                    VersionedRegistryCompositionError::ProviderDefinitionMismatch {
                        provider_id: provider_id.clone(),
                        revision: *provider_revision,
                    },
                );
            }
            let binding_id = provider.credential_binding_id.clone();
            let has_active_binding = self
                .inspect_credentials(&binding_id)
                .await?
                .into_iter()
                .any(|binding| binding.active);
            if !has_active_binding {
                return Err(
                    VersionedRegistryCompositionError::MissingActiveCredentialBinding {
                        provider_id: provider_id.clone(),
                        binding_id,
                    },
                );
            }
            providers.insert(provider_id.clone(), provider);
        }

        let mut models = BTreeMap::new();
        for binding in &snapshot.models {
            if !providers.contains_key(&binding.model.provider_id) {
                return Err(VersionedRegistryCompositionError::MissingProviderPin {
                    provider_id: binding.model.provider_id.clone(),
                    model: binding.model.clone(),
                });
            }
            let model = self
                .load_model_revision(
                    &binding.model.provider_id,
                    &binding.model.model_id,
                    binding.revision,
                )
                .await?
                .ok_or_else(|| VersionedRegistryCompositionError::UnknownModelRevision {
                    model: binding.model.clone(),
                    revision: binding.revision,
                })?
                .definition;
            if model.provider_id != binding.model.provider_id
                || model.model_id != binding.model.model_id
                || model.revision != binding.revision
            {
                return Err(VersionedRegistryCompositionError::ModelDefinitionMismatch {
                    model: binding.model.clone(),
                    revision: binding.revision,
                });
            }
            if models.insert(binding.model.clone(), model).is_some() {
                return Err(VersionedRegistryCompositionError::DuplicateModel(
                    binding.model.clone(),
                ));
            }
        }
        if !models.contains_key(&configured_default) {
            return Err(VersionedRegistryCompositionError::MissingDefaultModel(
                configured_default,
            ));
        }

        Ok(VersionedRegistryCompositionSnapshot {
            agent,
            providers,
            models,
            default_model: configured_default,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum VersionedRegistryCompositionError {
    #[error("unknown Agent revision `{agent_id}`@{revision}")]
    UnknownAgentRevision { agent_id: String, revision: u64 },
    #[error("stored Agent definition does not match `{agent_id}`@{revision}")]
    AgentDefinitionMismatch { agent_id: String, revision: u64 },
    #[error("Agent revision `{agent_id}`@{revision} has no immutable registry snapshot")]
    MissingSnapshot { agent_id: String, revision: u64 },
    #[error("registry snapshot does not match `{agent_id}`@{revision}")]
    SnapshotIdentityMismatch { agent_id: String, revision: u64 },
    #[error("unknown Provider revision `{provider_id}`@{revision}")]
    UnknownProviderRevision { provider_id: String, revision: u64 },
    #[error("stored Provider definition does not match `{provider_id}`@{revision}")]
    ProviderDefinitionMismatch { provider_id: String, revision: u64 },
    #[error("Provider `{provider_id}` credential binding `{binding_id}` has no active generation")]
    MissingActiveCredentialBinding {
        provider_id: String,
        binding_id: String,
    },
    #[error("Model `{model:?}` has no exact Provider `{provider_id}` pin")]
    MissingProviderPin {
        provider_id: String,
        model: ModelSelection,
    },
    #[error("unknown Model revision `{model:?}`@{revision}")]
    UnknownModelRevision {
        model: ModelSelection,
        revision: u64,
    },
    #[error("stored Model definition does not match `{model:?}`@{revision}")]
    ModelDefinitionMismatch {
        model: ModelSelection,
        revision: u64,
    },
    #[error("registry snapshot contains duplicate Model `{0:?}`")]
    DuplicateModel(ModelSelection),
    #[error("Agent default Model `{configured:?}` does not match snapshot `{snapshot:?}`")]
    DefaultModelMismatch {
        configured: ModelSelection,
        snapshot: ModelSelection,
    },
    #[error("registry composition does not contain default Model `{0:?}`")]
    MissingDefaultModel(ModelSelection),
    #[error(transparent)]
    Snapshot(#[from] AgentSnapshotV3Error),
    #[error(transparent)]
    Credential(#[from] CredentialRegistryError),
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
}
