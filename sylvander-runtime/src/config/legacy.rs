//! Compatibility conversion from the pre-config-file environment contract.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use sylvander_agent::spec::{AgentSpec, BehaviorConfig, ModelConfig, PersonaConfig, ToolRef};

use super::{
    AgentDefinitionConfig, ApprovalSettings, CONFIG_SCHEMA_VERSION, ChannelInstanceConfig,
    ChannelTransportConfig, ConfigError, ExecutionTargetConfig, ExecutionTransportConfig,
    ModelDefinitionConfig, ModelProviderConfig, SecretRef, ServerConfig, ServerSettings,
};

const LEGACY_KEYS: &[&str] = &[
    "SYLVANDER_MODEL",
    "SYLVANDER_MODELS",
    "SYLVANDER_REASONING_MODELS",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_BASE_URL",
    "SYLVANDER_SESSION_DB",
    "SYLVANDER_WORKSPACE_JOURNAL",
    "SYLVANDER_AGENT_WORKSPACE",
    "SYLVANDER_SOCKET",
    "HTTP_ADDR",
    "SYLVANDER_APPROVAL",
    "SYLVANDER_APPROVAL_STORE",
    "DINGTALK_APP_KEY",
    "DINGTALK_APP_SECRET",
    "TELEGRAM_BOT_TOKEN",
    "TELEGRAM_WEBHOOK_ADDR",
    "TELEGRAM_WEBHOOK_SECRET",
];

impl ServerConfig {
    /// Convert the legacy environment interface into the versioned domain
    /// model. Secret values are checked for presence but only their variable
    /// names are retained.
    pub fn from_legacy_env() -> Result<Self, ConfigError> {
        let values = LEGACY_KEYS
            .iter()
            .filter_map(|key| {
                std::env::var(key)
                    .ok()
                    .map(|value| ((*key).to_string(), value))
            })
            .collect();
        Self::from_legacy_values(&values)
    }

    pub(crate) fn from_legacy_values(
        values: &HashMap<String, String>,
    ) -> Result<Self, ConfigError> {
        let required = |key: &str| -> Result<String, ConfigError> {
            values
                .get(key)
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .ok_or_else(|| ConfigError {
                    errors: vec![format!("legacy environment requires non-empty {key}")],
                })
        };
        let model = required("SYLVANDER_MODEL")?;
        required("ANTHROPIC_API_KEY")?;
        let base_url = required("ANTHROPIC_BASE_URL")?;

        let mut models = comma_values(values.get("SYLVANDER_MODELS"));
        if !models.iter().any(|candidate| candidate == &model) {
            models.insert(0, model.clone());
        }
        let reasoning = comma_values(values.get("SYLVANDER_REASONING_MODELS"))
            .into_iter()
            .collect::<HashSet<_>>();
        let model_definitions = models
            .into_iter()
            .map(|id| ModelDefinitionConfig {
                capabilities: if reasoning.contains(&id) {
                    vec!["tool_use".into(), "vision".into(), "reasoning".into()]
                } else {
                    vec!["tool_use".into(), "vision".into()]
                },
                id,
                context_window: 200_000,
                max_output_tokens: 32_000,
            })
            .collect();

        let spec = AgentSpec::builder()
            .id("assistant")
            .name("Assistant")
            .persona(PersonaConfig {
                system_prompt: "You are a helpful assistant. Use tools carefully and ask the user when a decision or missing information blocks correct progress.".into(),
                description: "Default assistant".into(),
            })
            .model(ModelConfig {
                provider: "primary".into(),
                model_name: model,
                temperature: None,
                max_tokens: Some(32_000),
            })
            .tools(vec![
                ToolRef::Builtin { name: "read".into() },
                ToolRef::Builtin { name: "write".into() },
                ToolRef::Builtin { name: "edit".into() },
            ])
            .behavior(BehaviorConfig { max_iterations: 30, max_retries: 3 })
            .build()
            .map_err(|error| ConfigError {
                errors: vec![format!("failed to build legacy Agent: {error}")],
            })?;

        let mut channels = vec![
            ChannelInstanceConfig {
                id: "terminal".into(),
                enabled: true,
                default_agent: "assistant".into(),
                default_workspace: None,
                transport: ChannelTransportConfig::Unix {
                    path: values
                        .get("SYLVANDER_SOCKET")
                        .map_or_else(|| "/tmp/sylvander.sock".into(), PathBuf::from),
                },
            },
            ChannelInstanceConfig {
                id: "http-debug".into(),
                enabled: true,
                default_agent: "assistant".into(),
                default_workspace: None,
                transport: ChannelTransportConfig::Http {
                    bind: values
                        .get("HTTP_ADDR")
                        .cloned()
                        .unwrap_or_else(|| "127.0.0.1:8080".into()),
                },
            },
        ];
        add_legacy_dingtalk(values, &mut channels)?;
        add_legacy_telegram(values, &mut channels)?;

        let config = Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            server: ServerSettings {
                name: "sylvander".into(),
                data_dir: None,
                session_db: values.get("SYLVANDER_SESSION_DB").map(PathBuf::from),
                workspace_journal: values.get("SYLVANDER_WORKSPACE_JOURNAL").map(PathBuf::from),
                approval: ApprovalSettings {
                    enabled: values.contains_key("SYLVANDER_APPROVAL"),
                    persistent_store: values.get("SYLVANDER_APPROVAL_STORE").map(PathBuf::from),
                },
                evidence: super::EvidenceSettings::default(),
            },
            model_providers: vec![ModelProviderConfig {
                id: "primary".into(),
                kind: "anthropic_compatible".into(),
                base_url,
                api_key: SecretRef::Env {
                    name: "ANTHROPIC_API_KEY".into(),
                },
                models: model_definitions,
            }],
            execution_targets: vec![ExecutionTargetConfig {
                id: "server-local".into(),
                transport: ExecutionTransportConfig::Local { root: None },
            }],
            agents: vec![AgentDefinitionConfig {
                revision: 1,
                spec,
                agent_workspace: values.get("SYLVANDER_AGENT_WORKSPACE").map(|path| {
                    super::WorkspaceBindingConfig {
                        execution_target: "server-local".into(),
                        path: path.clone(),
                        read_only: false,
                    }
                }),
                prompt_profiles: Vec::new(),
                default_prompt_profile: None,
                allow_session_prompt: false,
            }],
            channels,
        };
        config.validate()?;
        Ok(config)
    }
}

fn comma_values(value: Option<&String>) -> Vec<String> {
    value
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(String::from)
        .collect()
}

fn configured_pair(
    values: &HashMap<String, String>,
    first: &str,
    second: &str,
) -> Result<bool, ConfigError> {
    let first_present = values
        .get(first)
        .is_some_and(|value| !value.trim().is_empty());
    let second_present = values
        .get(second)
        .is_some_and(|value| !value.trim().is_empty());
    if first_present != second_present {
        return Err(ConfigError {
            errors: vec![format!("legacy channel requires both {first} and {second}")],
        });
    }
    Ok(first_present)
}

fn add_legacy_dingtalk(
    values: &HashMap<String, String>,
    channels: &mut Vec<ChannelInstanceConfig>,
) -> Result<(), ConfigError> {
    if configured_pair(values, "DINGTALK_APP_KEY", "DINGTALK_APP_SECRET")? {
        channels.push(ChannelInstanceConfig {
            id: "dingtalk-default".into(),
            enabled: true,
            default_agent: "assistant".into(),
            default_workspace: None,
            transport: ChannelTransportConfig::DingTalk {
                app_key: SecretRef::Env {
                    name: "DINGTALK_APP_KEY".into(),
                },
                app_secret: SecretRef::Env {
                    name: "DINGTALK_APP_SECRET".into(),
                },
            },
        });
    }
    Ok(())
}

fn add_legacy_telegram(
    values: &HashMap<String, String>,
    channels: &mut Vec<ChannelInstanceConfig>,
) -> Result<(), ConfigError> {
    let Some(token) = values.get("TELEGRAM_BOT_TOKEN") else {
        return Ok(());
    };
    if token.trim().is_empty() {
        return Err(ConfigError {
            errors: vec!["legacy TELEGRAM_BOT_TOKEN is empty".into()],
        });
    }
    let webhook_secret = values
        .get("TELEGRAM_WEBHOOK_SECRET")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ConfigError {
            errors: vec!["legacy Telegram requires TELEGRAM_WEBHOOK_SECRET".into()],
        })?;
    let _ = webhook_secret;
    channels.push(ChannelInstanceConfig {
        id: "telegram-default".into(),
        enabled: true,
        default_agent: "assistant".into(),
        default_workspace: None,
        transport: ChannelTransportConfig::Telegram {
            token: SecretRef::Env {
                name: "TELEGRAM_BOT_TOKEN".into(),
            },
            bind: values
                .get("TELEGRAM_WEBHOOK_ADDR")
                .cloned()
                .unwrap_or_else(|| "127.0.0.1:8081".into()),
            webhook_secret: SecretRef::Env {
                name: "TELEGRAM_WEBHOOK_SECRET".into(),
            },
        },
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn required_values() -> HashMap<String, String> {
        HashMap::from([
            ("SYLVANDER_MODEL".into(), "model-a".into()),
            ("ANTHROPIC_API_KEY".into(), "must-not-be-stored".into()),
            (
                "ANTHROPIC_BASE_URL".into(),
                "https://models.example.test".into(),
            ),
        ])
    }

    #[test]
    fn conversion_keeps_secret_references_not_values() {
        let values = required_values();
        let config = ServerConfig::from_legacy_values(&values).unwrap();
        let encoded = toml::to_string(&config).unwrap();
        assert!(encoded.contains("ANTHROPIC_API_KEY"));
        assert!(!encoded.contains("must-not-be-stored"));
        assert_eq!(config.agents[0].spec.model.provider, "primary");
    }

    #[test]
    fn alternate_models_and_optional_channels_are_migrated() {
        let mut values = required_values();
        values.insert("SYLVANDER_MODELS".into(), "model-b, model-a".into());
        values.insert("DINGTALK_APP_KEY".into(), "key".into());
        values.insert("DINGTALK_APP_SECRET".into(), "secret".into());
        values.insert("TELEGRAM_BOT_TOKEN".into(), "token".into());
        values.insert("TELEGRAM_WEBHOOK_SECRET".into(), "webhook-secret".into());
        let config = ServerConfig::from_legacy_values(&values).unwrap();
        assert_eq!(config.model_providers[0].models.len(), 2);
        assert_eq!(config.channels.len(), 4);
        assert_eq!(config.channels[2].id, "dingtalk-default");
        assert_eq!(config.channels[3].id, "telegram-default");
    }

    #[test]
    fn partial_channel_credentials_fail_before_startup() {
        let mut values = required_values();
        values.insert("DINGTALK_APP_KEY".into(), "key".into());
        let error = ServerConfig::from_legacy_values(&values).unwrap_err();
        assert!(error.errors[0].contains("requires both"));
    }
}
