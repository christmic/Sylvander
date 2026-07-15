//! Pure, deterministic projection from bootstrap configuration to registry seeds.

use std::collections::BTreeSet;

use sylvander_protocol::ModelLifecycle;

use crate::agent_registry::AgentRegistry;
use crate::config::{ConfigError, ServerConfig};
use crate::credential_registry::CredentialRegistryError;
use crate::model_registry::ModelRegistryError;
use crate::provider_registry::ProviderRegistryError;
use crate::registry_domain::{CredentialBindingRevision, ModelDefinition, ProviderDefinition};

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RegistryBootstrapPlan {
    pub credentials: Vec<CredentialBindingRevision>,
    pub providers: Vec<ProviderDefinition>,
    pub models: Vec<ModelDefinition>,
}

impl std::fmt::Debug for RegistryBootstrapPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistryBootstrapPlan")
            .field(
                "credentials",
                &self
                    .credentials
                    .iter()
                    .map(|seed| (&seed.binding_id, seed.generation))
                    .collect::<Vec<_>>(),
            )
            .field("providers", &self.providers)
            .field("models", &self.models)
            .finish()
    }
}

impl RegistryBootstrapPlan {
    pub(crate) fn from_config(config: &ServerConfig) -> Result<Self, BootstrapPlanError> {
        config.validate()?;
        let mut credentials = Vec::with_capacity(config.model_providers.len());
        let mut providers = Vec::with_capacity(config.model_providers.len());
        let model_count = config
            .model_providers
            .iter()
            .map(|provider| provider.models.len())
            .sum();
        let mut models = Vec::with_capacity(model_count);

        for configured in &config.model_providers {
            let provider_id = configured.id.trim().to_owned();
            let binding_id = credential_binding_id(&provider_id);
            credentials.push(CredentialBindingRevision {
                binding_id: binding_id.clone(),
                generation: 1,
                reference: configured.api_key.clone(),
            });
            providers.push(ProviderDefinition {
                id: provider_id.clone(),
                revision: 1,
                kind: configured.kind.trim().to_owned(),
                base_url: configured.base_url.trim().to_owned(),
                credential_binding_id: binding_id,
            });
            for configured_model in &configured.models {
                models.push(ModelDefinition {
                    provider_id: provider_id.clone(),
                    model_id: configured_model.id.trim().to_owned(),
                    revision: 1,
                    context_window: configured_model.context_window,
                    max_output_tokens: configured_model.max_output_tokens,
                    capabilities: normalize_capabilities(
                        &provider_id,
                        configured_model.id.trim(),
                        &configured_model.capabilities,
                    )?,
                    lifecycle: ModelLifecycle::Active,
                    pricing: None,
                });
            }
        }
        credentials.sort_by(|left, right| left.binding_id.cmp(&right.binding_id));
        providers.sort_by(|left, right| left.id.cmp(&right.id));
        models.sort_by(|left, right| {
            (&left.provider_id, &left.model_id).cmp(&(&right.provider_id, &right.model_id))
        });
        Ok(Self {
            credentials,
            providers,
            models,
        })
    }
}

impl AgentRegistry {
    pub(crate) async fn bootstrap_registries(
        &self,
        config: &ServerConfig,
    ) -> Result<RegistryBootstrapReport, RegistryBootstrapError> {
        let plan = RegistryBootstrapPlan::from_config(config)?;
        let mut report = RegistryBootstrapReport::default();
        for seed in plan.credentials {
            let identity = seed.binding_id.clone();
            let existing = self.load_active_credential(&identity).await?;
            let active = self.seed_credential(seed).await?.definition.generation;
            report.entries.push(BootstrapEntry::new(
                RegistrySeedKind::Credential,
                identity,
                1,
                active,
                existing.is_some(),
            ));
        }
        for seed in plan.providers {
            let identity = seed.id.clone();
            let existing = self.load_active_provider(&identity).await?;
            let active = self.seed_provider(seed).await?.definition.revision;
            report.entries.push(BootstrapEntry::new(
                RegistrySeedKind::Provider,
                identity,
                1,
                active,
                existing.is_some(),
            ));
        }
        for seed in plan.models {
            let key = (seed.provider_id.as_str(), seed.model_id.as_str());
            let identity = format!("{}/{}", key.0, key.1);
            let existing = self.load_active_model(key).await?;
            let active = self.seed_model(seed).await?.definition.revision;
            report.entries.push(BootstrapEntry::new(
                RegistrySeedKind::Model,
                identity,
                1,
                active,
                existing.is_some(),
            ));
        }
        Ok(report)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegistrySeedKind {
    Credential,
    Provider,
    Model,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BootstrapOutcome {
    Seeded { active_version: u64 },
    ExistingPreserved { active_version: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BootstrapEntry {
    pub kind: RegistrySeedKind,
    pub identity: String,
    pub configured_version: u64,
    pub outcome: BootstrapOutcome,
}

impl BootstrapEntry {
    fn new(
        kind: RegistrySeedKind,
        identity: String,
        configured_version: u64,
        active_version: u64,
        existed: bool,
    ) -> Self {
        let outcome = if existed {
            BootstrapOutcome::ExistingPreserved { active_version }
        } else {
            BootstrapOutcome::Seeded { active_version }
        };
        Self {
            kind,
            identity,
            configured_version,
            outcome,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RegistryBootstrapReport {
    pub entries: Vec<BootstrapEntry>,
}

fn credential_binding_id(provider_id: &str) -> String {
    format!("provider:{provider_id}:api_key")
}

fn normalize_capabilities(
    provider_id: &str,
    model_id: &str,
    capabilities: &[String],
) -> Result<BTreeSet<String>, BootstrapPlanError> {
    capabilities
        .iter()
        .map(|capability| match capability.trim() {
            "reasoning" | "extended_thinking" => Ok("extended_thinking".to_owned()),
            "prompt_caching" | "structured_output" | "tool_use" | "vision" | "document_input" => {
                Ok(capability.trim().to_owned())
            }
            capability => Err(BootstrapPlanError::UnknownCapability {
                provider_id: provider_id.to_owned(),
                model_id: model_id.to_owned(),
                capability: capability.to_owned(),
            }),
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BootstrapPlanError {
    #[error(transparent)]
    InvalidConfig(#[from] ConfigError),
    #[error("model `{provider_id}/{model_id}` has unknown capability `{capability}`")]
    UnknownCapability {
        provider_id: String,
        model_id: String,
        capability: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RegistryBootstrapError {
    #[error(transparent)]
    Plan(#[from] BootstrapPlanError),
    #[error(transparent)]
    Credential(#[from] CredentialRegistryError),
    #[error(transparent)]
    Provider(#[from] ProviderRegistryError),
    #[error(transparent)]
    Model(#[from] ModelRegistryError),
}
