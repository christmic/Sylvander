//! Read-only reconciliation for optional provider model enumeration.

use std::collections::BTreeMap;

use sylvander_llm_core::{ModelInfo, ModelProvider, ModelRef, ProviderErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCatalogHealth {
    OperatorManaged,
    Synchronized,
    Drifted,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCatalogReport {
    pub health: ProviderCatalogHealth,
    pub missing_from_provider: Vec<ModelRef>,
    pub unexpected_from_provider: Vec<ModelRef>,
    pub metadata_drift: Vec<ModelRef>,
    pub failure_kind: Option<ProviderErrorKind>,
}

/// Compare optional remote discovery against Registry metadata without ever
/// mutating the Registry or an active Agent snapshot.
pub async fn inspect_provider_catalog(
    provider: &dyn ModelProvider,
    configured: &[ModelInfo],
) -> ProviderCatalogReport {
    let discovered = match provider.model_catalog().await {
        Ok(Some(models)) => models,
        Ok(None) => {
            return ProviderCatalogReport {
                health: ProviderCatalogHealth::OperatorManaged,
                missing_from_provider: vec![],
                unexpected_from_provider: vec![],
                metadata_drift: vec![],
                failure_kind: None,
            };
        }
        Err(error) => {
            return ProviderCatalogReport {
                health: ProviderCatalogHealth::Unavailable,
                missing_from_provider: vec![],
                unexpected_from_provider: vec![],
                metadata_drift: vec![],
                failure_kind: Some(error.kind),
            };
        }
    };
    let configured = by_identity(configured);
    let discovered = by_identity(&discovered);
    let missing_from_provider = configured
        .iter()
        .filter(|(key, _)| !discovered.contains_key(*key))
        .map(|(_, model)| model.reference.clone())
        .collect::<Vec<_>>();
    let unexpected_from_provider = discovered
        .iter()
        .filter(|(key, _)| !configured.contains_key(*key))
        .map(|(_, model)| model.reference.clone())
        .collect::<Vec<_>>();
    let metadata_drift = configured
        .iter()
        .filter_map(|(key, expected)| {
            discovered
                .get(key)
                .filter(|actual| *actual != expected)
                .map(|_| expected.reference.clone())
        })
        .collect::<Vec<_>>();
    let health = if missing_from_provider.is_empty()
        && unexpected_from_provider.is_empty()
        && metadata_drift.is_empty()
    {
        ProviderCatalogHealth::Synchronized
    } else {
        ProviderCatalogHealth::Drifted
    };
    ProviderCatalogReport {
        health,
        missing_from_provider,
        unexpected_from_provider,
        metadata_drift,
        failure_kind: None,
    }
}

fn by_identity(models: &[ModelInfo]) -> BTreeMap<String, &ModelInfo> {
    models
        .iter()
        .map(|model| {
            (
                format!("{}\0{}", model.reference.provider, model.reference.model),
                model,
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/unit/provider_catalog_sync.rs"]
mod tests;
