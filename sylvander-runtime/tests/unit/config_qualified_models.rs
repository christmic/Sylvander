use super::*;

fn qualified_config() -> ServerConfig {
    ServerConfig::from_toml(
        r#"
schema_version = 1

[server]
mode = "self_use"

[[model_providers]]
id = "alpha"
base_url = "https://alpha.example.invalid"
[model_providers.api_key]
source = "env"
name = "ALPHA_TOKEN"
[[model_providers.models]]
id = "shared"

[[model_providers]]
id = "beta"
base_url = "https://beta.example.invalid"
[model_providers.api_key]
source = "env"
name = "BETA_TOKEN"
[[model_providers.models]]
id = "shared"

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
allowed_models = [
  { provider_id = "alpha", model_id = "shared" },
  { provider_id = "beta", model_id = "shared" },
]
"#,
    )
    .expect("qualified configuration")
}

fn validation_text(config: &ServerConfig) -> String {
    config.validate().unwrap_err().errors.join("\n")
}

#[test]
fn qualified_allowlist_accepts_same_model_id_across_providers() {
    qualified_config().validate().unwrap();
}

#[test]
fn qualified_allowlist_must_be_explicit_and_non_empty() {
    let mut config = qualified_config();
    config.agents[0].spec.model.allowed_models.clear();
    assert!(
        validation_text(&config)
            .contains("allowed Models must be explicitly configured and non-empty")
    );
}

#[test]
fn qualified_allowlist_rejects_unknown_provider() {
    let mut config = qualified_config();
    config.agents[0].spec.model.allowed_models[1].provider_id = "missing".into();
    assert!(validation_text(&config).contains("references unknown provider missing"));
}

#[test]
fn qualified_allowlist_rejects_missing_model() {
    let mut config = qualified_config();
    config.agents[0].spec.model.allowed_models[1].model_id = "missing".into();
    assert!(validation_text(&config).contains("absent from provider beta"));
}

#[test]
fn qualified_allowlist_rejects_duplicate_exact_pair() {
    let mut config = qualified_config();
    let duplicate = config.agents[0].spec.model.allowed_models[0].clone();
    config.agents[0].spec.model.allowed_models[1] = duplicate;
    assert!(validation_text(&config).contains("duplicate allowed Model alpha/shared"));
}

#[test]
fn qualified_allowlist_rejects_missing_default_pair() {
    let mut config = qualified_config();
    config.agents[0].spec.model.allowed_models.remove(0);
    assert!(validation_text(&config).contains("do not contain its default alpha/shared"));
}
