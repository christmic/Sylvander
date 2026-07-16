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
mod tests {
    use futures_util::stream;
    use sylvander_llm_core::{ModelCapabilities, ModelEventStream, ModelRequest, ProviderFuture};

    use super::*;

    struct CatalogProvider(Vec<ModelInfo>);

    impl ModelProvider for CatalogProvider {
        fn complete_stream(&self, _request: ModelRequest) -> ProviderFuture<'_> {
            Box::pin(async {
                let stream: ModelEventStream = Box::pin(stream::empty());
                Ok(stream)
            })
        }

        fn model_catalog(&self) -> sylvander_llm_core::ModelCatalogFuture<'_> {
            let models = self.0.clone();
            Box::pin(async move { Ok(Some(models)) })
        }
    }

    #[tokio::test]
    async fn discovery_reports_drift_without_changing_registry_input() {
        let configured = vec![model("one", 100), model("two", 200)];
        let provider = CatalogProvider(vec![model("one", 90), model("three", 300)]);
        let report = inspect_provider_catalog(&provider, &configured).await;
        assert_eq!(report.health, ProviderCatalogHealth::Drifted);
        assert_eq!(
            report.missing_from_provider,
            vec![ModelRef::new("test", "two")]
        );
        assert_eq!(
            report.unexpected_from_provider,
            vec![ModelRef::new("test", "three")]
        );
        assert_eq!(report.metadata_drift, vec![ModelRef::new("test", "one")]);
        assert_eq!(configured[0].context_window, 100);
    }

    #[tokio::test]
    async fn providers_without_enumeration_remain_operator_managed() {
        struct ManagedProvider;
        impl ModelProvider for ManagedProvider {
            fn complete_stream(&self, _request: ModelRequest) -> ProviderFuture<'_> {
                Box::pin(async {
                    let stream: ModelEventStream = Box::pin(stream::empty());
                    Ok(stream)
                })
            }
        }
        let report = inspect_provider_catalog(&ManagedProvider, &[model("one", 100)]).await;
        assert_eq!(report.health, ProviderCatalogHealth::OperatorManaged);
    }

    fn model(id: &str, context_window: u32) -> ModelInfo {
        ModelInfo {
            reference: ModelRef::new("test", id),
            context_window,
            max_output_tokens: 10,
            capabilities: ModelCapabilities::TOOL_USE,
        }
    }
}
