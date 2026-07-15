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

[agents.access]
allow_authenticated = true

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
qualified_models = [{ provider_id = "primary", model_id = "model-a" }]
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
    assert_eq!(
        config.server.memory_maintenance,
        MemoryMaintenanceSettings::default()
    );
}

#[test]
fn memory_maintenance_policy_parses_strict_nested_configuration() {
    let input = valid_toml().replace(
        "data_dir = \"/var/lib/sylvander\"",
        r#"data_dir = "/var/lib/sylvander"

[server.memory_maintenance]
interval_seconds = 900
batch_size = 250
max_batches_per_run = 8

[server.memory_maintenance.retention]
default_ttl_days = 90
max_ttl_days = 730
expired_grace_days = 3
superseded_retention_days = 14"#,
    );
    let config = ServerConfig::from_toml(&input).unwrap();
    let maintenance = config.server.memory_maintenance;
    assert_eq!(maintenance.interval_seconds, 900);
    assert_eq!(maintenance.batch_size, 250);
    assert_eq!(maintenance.max_batches_per_run, 8);
    assert_eq!(maintenance.retention.default_ttl_days, 90);
    assert_eq!(maintenance.retention.max_ttl_days, 730);
    assert_eq!(maintenance.retention.expired_grace_days, 3);
    assert_eq!(maintenance.retention.superseded_retention_days, 14);
}

#[test]
fn memory_maintenance_rejects_unknown_and_unbounded_values() {
    for unknown in [
        "[server.memory_maintenance]\nbackup_directory = \"/tmp\"",
        "[server.memory_maintenance.retention]\nforever = true",
    ] {
        let input = valid_toml().replace(
            "data_dir = \"/var/lib/sylvander\"",
            &format!("data_dir = \"/var/lib/sylvander\"\n\n{unknown}"),
        );
        assert!(ServerConfig::from_toml(&input).unwrap_err().errors[0].contains("unknown field"));
    }

    let mut config = ServerConfig::from_toml(&valid_toml()).unwrap();
    let maintenance = &mut config.server.memory_maintenance;
    maintenance.retention.default_ttl_days = 0;
    maintenance.retention.max_ttl_days = 1_826;
    maintenance.retention.expired_grace_days = 366;
    maintenance.retention.superseded_retention_days = 0;
    maintenance.interval_seconds = 59;
    maintenance.batch_size = 0;
    maintenance.max_batches_per_run = 101;
    let joined = config.validate().unwrap_err().errors.join("\n");
    for field in [
        "default_ttl_days",
        "max_ttl_days",
        "expired_grace_days",
        "superseded_retention_days",
        "interval_seconds",
        "batch_size",
        "max_batches_per_run",
    ] {
        assert!(joined.contains(field), "missing validation for {field}");
    }

    let mut config = ServerConfig::from_toml(&valid_toml()).unwrap();
    config.server.memory_maintenance.retention.default_ttl_days = 31;
    config.server.memory_maintenance.retention.max_ttl_days = 30;
    assert!(
        config
            .validate()
            .unwrap_err()
            .errors
            .iter()
            .any(|error| error.contains("must not exceed max_ttl_days"))
    );
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
fn boot_rejects_noncanonical_and_oversized_prompt_inputs() {
    let mut cases = Vec::new();

    let mut spaced_id = ServerConfig::from_toml(&valid_toml()).unwrap();
    spaced_id.agents[0].prompt_profiles[0].id = " model-a".into();
    cases.push(spaced_id);

    let mut duplicate_selector = ServerConfig::from_toml(&valid_toml()).unwrap();
    duplicate_selector.agents[0].prompt_profiles[0].providers =
        vec!["primary".into(), "primary".into()];
    cases.push(duplicate_selector);

    let mut too_many_profiles = ServerConfig::from_toml(&valid_toml()).unwrap();
    let profile = too_many_profiles.agents[0].prompt_profiles[0].clone();
    too_many_profiles.agents[0].prompt_profiles = (0..33)
        .map(|index| PromptProfileConfig {
            id: format!("profile-{index}"),
            ..profile.clone()
        })
        .collect();
    too_many_profiles.agents[0].default_prompt_profile = None;
    cases.push(too_many_profiles);

    let mut oversized = ServerConfig::from_toml(&valid_toml()).unwrap();
    oversized.agents[0].spec.persona.system_prompt = "x".repeat(64 * 1024 + 1);
    cases.push(oversized);

    let mut forbidden_control = ServerConfig::from_toml(&valid_toml()).unwrap();
    forbidden_control.agents[0].prompt_profiles[0].system_prompt = "private\0prompt".into();
    cases.push(forbidden_control);

    for config in cases {
        let error = config.validate().unwrap_err();
        let rendered = error.errors.join("\n");
        assert!(rendered.contains("prompt configuration is invalid"));
        assert!(!rendered.contains("private\0prompt"));
    }
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

#[test]
fn maintained_example_configuration_stays_valid() {
    let input = include_str!("../../../config/sylvander.example.toml");
    ServerConfig::from_toml(input).expect("maintained example must parse and validate");
}

#[test]
fn evidence_capture_is_bounded_and_metadata_only_by_default() {
    let mut config = ServerConfig::from_toml(&valid_toml()).unwrap();
    assert_eq!(
        config.server.evidence.content,
        EvidenceContentPolicy::MetadataOnly
    );
    config.server.evidence.retention_days = 0;
    let error = config.validate().unwrap_err();
    assert!(
        error
            .errors
            .iter()
            .any(|message| message.contains("retention_days"))
    );
    config.server.evidence.retention_days = 30;
    config.server.boundary.max_request_bytes = 100;
    config.server.boundary.requests_per_minute = 0;
    let error = config.validate().unwrap_err();
    let joined = error.errors.join("\n");
    assert!(joined.contains("max_request_bytes"));
    assert!(joined.contains("requests_per_minute"));
}

#[test]
fn websocket_requires_a_principal_and_secret_reference() {
    let websocket = r#"

[[channels]]
id = "desktop-primary"
enabled = true
default_agent = "assistant"

[channels.transport]
kind = "websocket"
bind = "127.0.0.1:9080"
principal_id = "desktop-owner"

[channels.transport.bearer_token]
source = "env"
name = "SYLVANDER_DESKTOP_TOKEN"
"#;
    let config = ServerConfig::from_toml(&(valid_toml() + websocket))
        .expect("authenticated WebSocket configuration");
    assert!(matches!(
        &config.channels[1].transport,
        ChannelTransportConfig::Websocket {
            principal_id,
            bearer_token: SecretRef::Env { name },
            ..
        } if principal_id == "desktop-owner" && name == "SYLVANDER_DESKTOP_TOKEN"
    ));

    let missing_token = websocket.replace(
        "\n[channels.transport.bearer_token]\nsource = \"env\"\nname = \"SYLVANDER_DESKTOP_TOKEN\"",
        "",
    );
    let error = ServerConfig::from_toml(&(valid_toml() + &missing_token)).unwrap_err();
    assert!(error.errors[0].contains("missing field `bearer_token`"));
}
