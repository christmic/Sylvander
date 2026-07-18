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
