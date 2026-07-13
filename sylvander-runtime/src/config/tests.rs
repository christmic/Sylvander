use super::*;

fn valid_toml() -> String {
    r#"
schema_version = 1

[server]
name = "test-sylvander"
data_dir = "/var/lib/sylvander"

[[model_providers]]
id = "primary"
kind = "anthropic_compatible"
base_url = "https://models.example.test"

[model_providers.api_key]
source = "env"
name = "MODEL_API_KEY"

[[model_providers.models]]
id = "model-a"
context_window = 200000
max_output_tokens = 32000
capabilities = ["tool_use"]

[[execution_targets]]
id = "local"

[execution_targets.transport]
kind = "local"
root = "/workspace"

[[agents]]
revision = 7
default_prompt_profile = "model-a"
allow_session_prompt = false

[agents.spec]
id = "assistant"
name = "Sylvander"

[agents.spec.persona]
system_prompt = "Shared Agent prompt"
description = "Coding Agent"

[agents.spec.model]
provider = "primary"
model_name = "model-a"
max_tokens = 32000

[agents.agent_workspace]
execution_target = "local"
path = "/workspace/agent-home"
read_only = false

[[agents.prompt_profiles]]
id = "model-a"
providers = ["primary"]
models = ["model-a"]
system_prompt = "Model-specific prompt"

[[channels]]
id = "terminal"
enabled = true
default_agent = "assistant"

[channels.transport]
kind = "unix"
path = "/tmp/sylvander.sock"
"#
    .into()
}

#[test]
fn valid_configuration_parses_and_resolves_references() {
    let config = ServerConfig::from_toml(&valid_toml()).expect("valid configuration");
    assert_eq!(config.schema_version, CONFIG_SCHEMA_VERSION);
    assert_eq!(config.agents[0].spec.id.0, "assistant");
    assert_eq!(config.agents[0].revision, 7);
    assert_eq!(config.channels[0].id, "terminal");
    assert!(matches!(
        config.model_providers[0].api_key,
        SecretRef::Env { ref name } if name == "MODEL_API_KEY"
    ));
}

#[test]
fn unknown_fields_fail_instead_of_being_silently_ignored() {
    let input = valid_toml().replace(
        "name = \"test-sylvander\"",
        "name = \"test-sylvander\"\nunknown_option = true",
    );
    let error = ServerConfig::from_toml(&input).unwrap_err();
    assert!(error.errors[0].contains("unknown field `unknown_option`"));
}

#[test]
fn validation_collects_duplicate_and_dangling_references() {
    let mut config = ServerConfig::from_toml(&valid_toml()).unwrap();
    config
        .model_providers
        .push(config.model_providers[0].clone());
    config.agents[0].spec.model.model_name = "missing-model".into();
    config.agents[0]
        .agent_workspace
        .as_mut()
        .unwrap()
        .execution_target = "missing-target".into();
    config.channels[0].default_agent = "missing-agent".into();

    let error = config.validate().unwrap_err();
    let joined = error.errors.join("\n");
    assert!(joined.contains("duplicate model provider id `primary`"));
    assert!(joined.contains("references model missing-model absent from provider primary"));
    assert!(joined.contains("unknown execution target missing-target"));
    assert!(joined.contains("references unknown Agent missing-agent"));
}

#[test]
fn secret_values_cannot_be_embedded_inline() {
    let input = valid_toml().replace(
        "source = \"env\"\nname = \"MODEL_API_KEY\"",
        "source = \"literal\"\nvalue = \"do-not-store-me\"",
    );
    let error = ServerConfig::from_toml(&input).unwrap_err();
    assert!(error.errors[0].contains("unknown variant `literal`"));
}

#[test]
fn invalid_schema_and_empty_secret_reference_are_rejected() {
    let mut config = ServerConfig::from_toml(&valid_toml()).unwrap();
    config.schema_version = 99;
    config.model_providers[0].api_key = SecretRef::Env { name: "  ".into() };
    let error = config.validate().unwrap_err();
    let joined = error.errors.join("\n");
    assert!(joined.contains("unsupported schema_version 99"));
    assert!(joined.contains("environment variable name is empty"));
}

#[test]
fn oversized_configuration_is_rejected_before_parsing() {
    let error = ServerConfig::from_toml(&"x".repeat(1024 * 1024 + 1)).unwrap_err();
    assert!(error.errors[0].contains("configuration exceeds"));
}
