//! Pure, deterministic projection from bootstrap configuration to registry seeds.

use sylvander_protocol::ModelLifecycle;

use crate::agent_registry::AgentRegistry;
use crate::config::{ConfigError, ServerConfig};
use crate::credential_registry::CredentialRegistryError;
use crate::model_registry::ModelRegistryError;
use crate::provider_registry::ProviderRegistryError;
use crate::registry_domain::{
    CredentialBindingRevision, ModelCapabilityError, ModelDefinition, ProviderDefinition,
    canonicalize_model_capabilities,
};

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
                    capabilities: canonicalize_model_capabilities(&configured_model.capabilities)
                        .map_err(|error| {
                        bootstrap_capability_error(&provider_id, configured_model.id.trim(), error)
                    })?,
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

fn bootstrap_capability_error(
    provider_id: &str,
    model_id: &str,
    error: ModelCapabilityError,
) -> BootstrapPlanError {
    let (capability, reason) = match error {
        ModelCapabilityError::Unknown(capability) => {
            return BootstrapPlanError::UnknownCapability {
                provider_id: provider_id.to_owned(),
                model_id: model_id.to_owned(),
                capability,
            };
        }
        ModelCapabilityError::Blank => (String::new(), BootstrapCapabilityIssue::Blank),
        ModelCapabilityError::SurroundingWhitespace(capability) => {
            (capability, BootstrapCapabilityIssue::SurroundingWhitespace)
        }
        ModelCapabilityError::NotLowercase(capability) => {
            (capability, BootstrapCapabilityIssue::NotLowercase)
        }
        ModelCapabilityError::Duplicate(capability) => (
            capability.as_str().to_owned(),
            BootstrapCapabilityIssue::Duplicate,
        ),
    };
    BootstrapPlanError::InvalidCapability {
        provider_id: provider_id.to_owned(),
        model_id: model_id.to_owned(),
        capability,
        reason,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BootstrapCapabilityIssue {
    #[error("must not be blank")]
    Blank,
    #[error("has surrounding whitespace")]
    SurroundingWhitespace,
    #[error("must use lowercase canonical spelling")]
    NotLowercase,
    #[error("duplicates another capability")]
    Duplicate,
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
    #[error("model `{provider_id}/{model_id}` has invalid capability `{capability}`: {reason}")]
    InvalidCapability {
        provider_id: String,
        model_id: String,
        capability: String,
        reason: BootstrapCapabilityIssue,
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

#[cfg(test)]
mod capability_tests {
    use super::*;

    fn config_with_capabilities(capabilities: &[&str]) -> ServerConfig {
        let mut config = ServerConfig::from_toml(
            r#"
schema_version = 1

[[model_providers]]
id = "provider"
base_url = "https://provider.invalid"
[model_providers.api_key]
source = "env"
name = "PROVIDER_API_KEY"
[[model_providers.models]]
id = "model"
context_window = 100
max_output_tokens = 10
"#,
        )
        .unwrap();
        config.model_providers[0].models[0].capabilities = capabilities
            .iter()
            .map(|value| (*value).to_owned())
            .collect();
        config
    }

    #[test]
    fn bootstrap_uses_canonical_domain_vocabulary() {
        let plan = RegistryBootstrapPlan::from_config(&config_with_capabilities(&[
            "reasoning",
            "tool_use",
        ]))
        .unwrap();
        assert_eq!(
            plan.models[0].capabilities,
            std::collections::BTreeSet::from([
                "extended_thinking".to_owned(),
                "tool_use".to_owned(),
            ])
        );
    }

    #[test]
    fn unknown_capability_keeps_the_bootstrap_error_boundary() {
        assert!(matches!(
            RegistryBootstrapPlan::from_config(&config_with_capabilities(&["telepathy"])),
            Err(BootstrapPlanError::UnknownCapability {
                provider_id,
                model_id,
                capability,
            }) if provider_id == "provider" && model_id == "model" && capability == "telepathy"
        ));
    }

    #[test]
    fn malformed_capabilities_map_to_typed_bootstrap_errors() {
        for (capability, expected) in [
            ("", BootstrapCapabilityIssue::Blank),
            ("   ", BootstrapCapabilityIssue::Blank),
            (" tool_use", BootstrapCapabilityIssue::SurroundingWhitespace),
            ("TOOL_USE", BootstrapCapabilityIssue::NotLowercase),
            ("Reasoning", BootstrapCapabilityIssue::NotLowercase),
        ] {
            assert!(matches!(
                RegistryBootstrapPlan::from_config(&config_with_capabilities(&[capability])),
                Err(BootstrapPlanError::InvalidCapability { reason, .. }) if reason == expected
            ));
        }
    }

    #[test]
    fn raw_and_alias_semantic_duplicates_are_rejected() {
        for capabilities in [
            &["tool_use", "tool_use"][..],
            &["reasoning", "extended_thinking"][..],
        ] {
            assert!(matches!(
                RegistryBootstrapPlan::from_config(&config_with_capabilities(capabilities)),
                Err(BootstrapPlanError::InvalidCapability {
                    reason: BootstrapCapabilityIssue::Duplicate,
                    ..
                })
            ));
        }
    }
}
