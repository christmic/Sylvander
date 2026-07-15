use std::collections::BTreeSet;

use sylvander_protocol::ModelLifecycle;
use tempfile::tempdir;

use crate::agent_registry::AgentRegistry;
use crate::config::{SecretRef, ServerConfig};
use crate::registry_bootstrap::{
    BootstrapOutcome, BootstrapPlanError, RegistryBootstrapPlan, RegistrySeedKind,
};

fn config() -> ServerConfig {
    ServerConfig::from_toml(
        r#"
schema_version = 1

[[model_providers]]
id = "zeta"
base_url = "https://zeta.invalid"
[model_providers.api_key]
source = "env"
name = "ZETA_SUPER_SECRET_VARIABLE"
[[model_providers.models]]
id = "shared"
context_window = 200
max_output_tokens = 20
capabilities = ["tool_use", "reasoning", "tool_use"]

[[model_providers]]
id = "alpha"
base_url = "https://alpha.invalid"
[model_providers.api_key]
source = "file"
path = "/private/alpha-secret"
[[model_providers.models]]
id = "shared"
context_window = 100
max_output_tokens = 10
capabilities = ["vision"]

[[agents]]
[agents.spec]
id = "assistant"
name = "Sylvander"
[agents.spec.model]
provider = "alpha"
model_name = "shared"
"#,
    )
    .unwrap()
}

#[test]
fn projection_is_sorted_qualified_normalized_and_redacted() {
    let mut reversed = config();
    let expected = RegistryBootstrapPlan::from_config(&reversed).unwrap();
    reversed.model_providers.reverse();
    assert_eq!(
        RegistryBootstrapPlan::from_config(&reversed).unwrap(),
        expected
    );
    assert_eq!(
        expected
            .credentials
            .iter()
            .map(|seed| seed.binding_id.as_str())
            .collect::<Vec<_>>(),
        ["provider:alpha:api_key", "provider:zeta:api_key"]
    );
    assert!(expected.credentials.iter().all(|seed| seed.generation == 1));
    assert_eq!(
        expected
            .models
            .iter()
            .map(|seed| (seed.provider_id.as_str(), seed.model_id.as_str()))
            .collect::<Vec<_>>(),
        [("alpha", "shared"), ("zeta", "shared")]
    );
    let zeta = &expected.models[1];
    assert_eq!(
        zeta.capabilities,
        BTreeSet::from(["extended_thinking".into(), "tool_use".into()])
    );
    assert_eq!(zeta.lifecycle, ModelLifecycle::Active);
    assert!(zeta.pricing.is_none());
    let debug = format!("{expected:?}");
    assert!(!debug.contains("ZETA_SUPER_SECRET_VARIABLE"));
    assert!(!debug.contains("/private/alpha-secret"));
}

#[test]
fn invalid_cross_reference_and_capability_fail_before_projection() {
    let mut invalid = config();
    invalid.agents[0].spec.model.provider = "missing".into();
    assert!(matches!(
        RegistryBootstrapPlan::from_config(&invalid),
        Err(BootstrapPlanError::InvalidConfig(_))
    ));
    let mut capability = config();
    capability.model_providers[0].models[0]
        .capabilities
        .push("telepathy".into());
    let error = RegistryBootstrapPlan::from_config(&capability).unwrap_err();
    assert!(matches!(
        &error,
        BootstrapPlanError::UnknownCapability { capability, .. } if capability == "telepathy"
    ));
    assert!(!error.to_string().contains("ZETA_SUPER_SECRET_VARIABLE"));
}

#[tokio::test]
async fn invalid_cross_reference_stops_before_runtime_storage_is_created() {
    let directory = tempdir().unwrap();
    let data_dir = directory.path().join("must-not-exist");
    let mut invalid = config();
    invalid.server.data_dir = Some(data_dir.clone());
    invalid.agents[0].spec.model.provider = "missing".into();
    let Err(error) = crate::Runtime::boot_config(invalid).await else {
        panic!("invalid cross-reference booted a runtime");
    };
    assert!(error.to_string().contains("unknown model provider missing"));
    assert!(!data_dir.exists());
}

#[tokio::test]
async fn first_boot_and_restart_are_retry_safe_without_overwrite() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let initial = registry.bootstrap_registries(&config()).await.unwrap();
    assert_eq!(initial.entries.len(), 6);
    assert!(initial.entries.iter().all(|entry| matches!(
        entry.outcome,
        BootstrapOutcome::Seeded { active_version: 1 }
    )));
    let provider = registry
        .load_active_provider("alpha")
        .await
        .unwrap()
        .unwrap();
    let model = registry
        .load_active_model(("alpha", "shared"))
        .await
        .unwrap()
        .unwrap();

    let mut drifted = config();
    let alpha = drifted
        .model_providers
        .iter_mut()
        .find(|provider| provider.id == "alpha")
        .unwrap();
    alpha.base_url = "https://must-not-replace.invalid".into();
    alpha.api_key = SecretRef::Env {
        name: "MUST_NOT_REPLACE".into(),
    };
    alpha.models[0].context_window = 999;
    let restart = registry.bootstrap_registries(&drifted).await.unwrap();
    assert!(restart.entries.iter().all(|entry| matches!(
        entry.outcome,
        BootstrapOutcome::ExistingPreserved { active_version: 1 }
    )));
    assert_eq!(
        registry
            .load_active_provider("alpha")
            .await
            .unwrap()
            .unwrap(),
        provider
    );
    assert_eq!(
        registry
            .load_active_model(("alpha", "shared"))
            .await
            .unwrap()
            .unwrap(),
        model
    );
}

#[tokio::test]
async fn active_revision_two_survives_and_new_identity_seeds() {
    let directory = tempdir().unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    let config = config();
    registry.bootstrap_registries(&config).await.unwrap();
    let plan = RegistryBootstrapPlan::from_config(&config).unwrap();
    let mut credential = plan.credentials[0].clone();
    credential.generation = 2;
    credential.reference = SecretRef::Env {
        name: "ROTATED_REFERENCE".into(),
    };
    registry.stage_credential(1, credential).await.unwrap();
    registry
        .activate_credential("provider:alpha:api_key", 2, 1)
        .await
        .unwrap();
    let mut provider = plan.providers[0].clone();
    provider.revision = 2;
    provider.base_url = "https://alpha-v2.invalid".into();
    registry.stage_provider(1, provider).await.unwrap();
    registry.activate_provider("alpha", 2, 1).await.unwrap();
    let mut model = plan.models[0].clone();
    model.revision = 2;
    model.context_window = 120;
    registry.stage_model(1, model).await.unwrap();
    registry
        .activate_model(("alpha", "shared"), 2, 1)
        .await
        .unwrap();

    let restart = registry.bootstrap_registries(&config).await.unwrap();
    for kind in [
        RegistrySeedKind::Credential,
        RegistrySeedKind::Provider,
        RegistrySeedKind::Model,
    ] {
        let entry = restart
            .entries
            .iter()
            .find(|entry| entry.kind == kind && entry.identity.contains("alpha"))
            .unwrap();
        assert_eq!(
            entry.outcome,
            BootstrapOutcome::ExistingPreserved { active_version: 2 }
        );
    }

    let mut expanded = config;
    let mut beta = expanded.model_providers[1].clone();
    beta.id = "beta".into();
    beta.models[0].id = "new-model".into();
    expanded.model_providers.push(beta);
    let report = registry.bootstrap_registries(&expanded).await.unwrap();
    assert!(report.entries.iter().any(|entry| {
        entry.identity == "beta/new-model"
            && entry.outcome == BootstrapOutcome::Seeded { active_version: 1 }
    }));
}
