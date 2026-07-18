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
    let plan =
        RegistryBootstrapPlan::from_config(&config_with_capabilities(&["reasoning", "tool_use"]))
            .unwrap();
    assert_eq!(
        plan.models[0].capabilities,
        std::collections::BTreeSet::from(["extended_thinking".to_owned(), "tool_use".to_owned(),])
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
