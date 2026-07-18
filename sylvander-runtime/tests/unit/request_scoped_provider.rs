use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sylvander_llm_core::{
    ChatMessage, ModelCapabilities, ModelProvider, ModelRef, ModelRequest, ProviderError,
    ProviderErrorKind, ProviderErrorPhase, ToolDefinition,
};
use tempfile::tempdir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::agent_registry::AgentRegistry;
use crate::config::{SecretRef, SystemSecretResolver};
use crate::registry_domain::{CredentialBindingRevision, ModelDefinition};

const TEXT_STREAM: &str = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-test\",\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":0}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

fn request(provider: &str) -> ModelRequest {
    qualified_request(provider, "claude-test")
}

fn qualified_request(provider: &str, model: &str) -> ModelRequest {
    ModelRequest {
        request_id: "request-1".into(),
        model: ModelRef::new(provider, model),
        system: Vec::new(),
        messages: vec![ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 16,
        reasoning: None,
        output_schema: None,
    }
}

struct RouteProbe {
    calls: AtomicUsize,
    requests: Mutex<Vec<ModelRef>>,
    failure: ProviderError,
}

impl RouteProbe {
    fn named(message: &'static str) -> Self {
        Self::failing(ProviderError::new(
            ProviderErrorKind::Other,
            ProviderErrorPhase::Open,
            message,
        ))
    }

    fn failing(error: ProviderError) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            failure: error,
        }
    }
}

impl ModelProvider for RouteProbe {
    fn complete_stream(&self, request: ModelRequest) -> ProviderFuture<'_> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.model.clone());
        let failure = self.failure.clone();
        Box::pin(async move { Err(failure) })
    }
}

fn router(
    routes: impl IntoIterator<Item = (&'static str, Arc<RouteProbe>)>,
    models: impl IntoIterator<Item = ModelRef>,
) -> Result<PinnedProviderRouter, ProviderRouterBuildError> {
    router_with_capabilities(
        routes,
        models.into_iter().map(|model| (model, all_capabilities())),
    )
}

fn router_with_capabilities(
    routes: impl IntoIterator<Item = (&'static str, Arc<RouteProbe>)>,
    models: impl IntoIterator<Item = (ModelRef, ModelCapabilities)>,
) -> Result<PinnedProviderRouter, ProviderRouterBuildError> {
    let routes = routes
        .into_iter()
        .map(|(provider_id, adapter)| (provider_id.to_string(), adapter as Arc<dyn ModelProvider>))
        .collect::<HashMap<_, _>>();
    PinnedProviderRouter::new(routes, models.into_iter().collect())
}

fn all_capabilities() -> ModelCapabilities {
    ModelCapabilities::REASONING
        | ModelCapabilities::PROMPT_CACHING
        | ModelCapabilities::STRUCTURED_OUTPUT
        | ModelCapabilities::TOOL_USE
        | ModelCapabilities::VISION
        | ModelCapabilities::DOCUMENT_INPUT
}

fn tool_request(provider: &str) -> ModelRequest {
    let mut request = qualified_request(provider, "shared");
    request.request_id = "secret-request-id".into();
    request.tools.push(ToolDefinition {
        name: "secret-tool".into(),
        description: "secret-description".into(),
        input_schema: serde_json::json!({"secret": true}),
        cache_hint: None,
    });
    request
}

async fn expect_key(server: &MockServer, key: &str, count: u64) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("authorization", format!("Bearer {key}")))
        .respond_with(ResponseTemplate::new(200).set_body_raw(TEXT_STREAM, "text/event-stream"))
        .expect(count)
        .mount(server)
        .await;
}

struct ProbeLease {
    generation: u64,
    value: String,
    drops: Arc<AtomicUsize>,
}

impl ActiveCredentialLease for ProbeLease {
    fn generation(&self) -> u64 {
        self.generation
    }

    fn secret(&self) -> Result<&str, CredentialAccessError> {
        Ok(&self.value)
    }
}

impl Drop for ProbeLease {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

enum MockResult {
    Lease(ProbeLease),
    Error(CredentialAccessError),
}

struct MockSource {
    calls: AtomicUsize,
    bindings: Mutex<Vec<String>>,
    results: Mutex<VecDeque<MockResult>>,
}

impl MockSource {
    fn new(results: impl IntoIterator<Item = MockResult>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            bindings: Mutex::new(Vec::new()),
            results: Mutex::new(results.into_iter().collect()),
        }
    }
}

impl ActiveCredentialSource for MockSource {
    fn resolve_active<'a>(&'a self, binding_id: &'a str) -> CredentialLeaseFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.bindings.lock().unwrap().push(binding_id.into());
            match self.results.lock().unwrap().pop_front().unwrap() {
                MockResult::Lease(lease) => Ok(Box::new(lease) as Box<dyn ActiveCredentialLease>),
                MockResult::Error(error) => Err(error),
            }
        })
    }
}

fn lease(generation: u64, value: &str, drops: &Arc<AtomicUsize>) -> MockResult {
    MockResult::Lease(ProbeLease {
        generation,
        value: value.into(),
        drops: drops.clone(),
    })
}

fn provider(
    server: &MockServer,
    credentials: Arc<dyn ActiveCredentialSource>,
) -> RequestScopedAnthropicProvider {
    RequestScopedAnthropicProvider::new(
        "anthropic",
        7,
        server.uri(),
        "provider:anthropic:api_key",
        credentials,
    )
}

fn stored_provider(revision: u64, kind: &str, base_url: String) -> ProviderDefinition {
    ProviderDefinition {
        id: "anthropic".into(),
        revision,
        kind: kind.into(),
        base_url,
        credential_binding_id: "provider:anthropic:api_key".into(),
    }
}

fn stored_model(provider_id: &str, capabilities: &[&str]) -> ModelDefinition {
    ModelDefinition {
        provider_id: provider_id.into(),
        model_id: "claude-test".into(),
        revision: 1,
        context_window: 200_000,
        max_output_tokens: 32_000,
        capabilities: capabilities
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect(),
        lifecycle: sylvander_protocol::ModelLifecycle::Active,
        pricing: None,
    }
}

#[tokio::test]
async fn every_open_resolves_again_and_releases_its_lease() {
    let server = MockServer::start().await;
    expect_key(&server, "first-key", 1).await;
    expect_key(&server, "second-key", 1).await;
    let drops = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(MockSource::new([
        lease(1, "first-key", &drops),
        lease(2, "second-key", &drops),
    ]));
    let provider = provider(&server, source.clone());

    let _first = provider
        .complete_stream(request("anthropic"))
        .await
        .unwrap();
    let _second = provider
        .complete_stream(request("anthropic"))
        .await
        .unwrap();

    assert_eq!(source.calls.load(Ordering::SeqCst), 2);
    assert_eq!(drops.load(Ordering::SeqCst), 2);
    server.verify().await;
}

#[tokio::test]
async fn registry_source_refreshes_files_and_rotates_generations() {
    let directory = tempdir().unwrap();
    let secret_path = directory.path().join("provider.secret");
    let rotated_path = directory.path().join("provider-rotated.secret");
    std::fs::write(&secret_path, "file-key-one\n").unwrap();
    std::fs::write(&rotated_path, "rotated-key\n").unwrap();
    let registry = AgentRegistry::open(directory.path().join("registry.db"))
        .await
        .unwrap();
    registry
        .seed_credential(CredentialBindingRevision {
            binding_id: "provider:anthropic:api_key".into(),
            generation: 1,
            reference: SecretRef::File {
                path: PathBuf::from(&secret_path),
            },
        })
        .await
        .unwrap();
    let server = MockServer::start().await;
    expect_key(&server, "file-key-one", 1).await;
    expect_key(&server, "file-key-two", 1).await;
    expect_key(&server, "rotated-key", 1).await;
    let source: Arc<dyn ActiveCredentialSource> = Arc::new(RegistryCredentialSource::new(
        registry.clone(),
        Arc::new(SystemSecretResolver),
    ));
    let provider = provider(&server, source);

    let _first = provider
        .complete_stream(request("anthropic"))
        .await
        .unwrap();
    std::fs::write(&secret_path, "file-key-two\n").unwrap();
    let _second = provider
        .complete_stream(request("anthropic"))
        .await
        .unwrap();
    registry
        .stage_credential(
            1,
            CredentialBindingRevision {
                binding_id: "provider:anthropic:api_key".into(),
                generation: 2,
                reference: SecretRef::File { path: rotated_path },
            },
        )
        .await
        .unwrap();
    registry
        .activate_credential("provider:anthropic:api_key", 2, 1)
        .await
        .unwrap();
    let _third = provider
        .complete_stream(request("anthropic"))
        .await
        .unwrap();

    server.verify().await;
}

#[tokio::test]
async fn resolution_failure_is_redacted_and_never_falls_back() {
    let server = MockServer::start().await;
    let drops = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(MockSource::new([
        MockResult::Error(CredentialAccessError::Unavailable),
        lease(1, "old-key-must-not-be-used", &drops),
    ]));
    let provider = provider(&server, source.clone());

    let Err(error) = provider.complete_stream(request("anthropic")).await else {
        panic!("expected credential resolution failure");
    };

    assert_eq!(error.kind, ProviderErrorKind::Authentication);
    assert_eq!(error.phase, ProviderErrorPhase::Open);
    assert_eq!(error.message, "provider credential unavailable");
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!format!("{error:?}").contains("old-key-must-not-be-used"));
}

#[tokio::test]
async fn provider_mismatch_does_not_resolve_a_credential() {
    let server = MockServer::start().await;
    let source = Arc::new(MockSource::new([MockResult::Error(
        CredentialAccessError::Unavailable,
    )]));
    let provider = provider(&server, source.clone());

    let Err(error) = provider.complete_stream(request("different")).await else {
        panic!("expected provider mismatch");
    };

    assert_eq!(error.kind, ProviderErrorKind::InvalidRequest);
    assert_eq!(error.message, "model provider does not match adapter");
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[test]
fn boundaries_are_object_safe_and_debug_is_redacted() {
    fn accepts_source(_: Arc<dyn ActiveCredentialSource>) {}
    fn accepts_provider(_: Arc<dyn ModelProvider>) {}
    fn accepts_factory(_: Arc<dyn ProviderAdapterFactory>) {}

    let source: Arc<dyn ActiveCredentialSource> = Arc::new(MockSource::new([]));
    accepts_source(source.clone());
    let provider = Arc::new(RequestScopedAnthropicProvider::new(
        "anthropic",
        3,
        "https://user:password@example.invalid",
        "secret-binding-name",
        source,
    ));
    let debug = format!("{provider:?}");
    assert!(!debug.contains("password"));
    assert!(!debug.contains("secret-binding-name"));
    accepts_provider(provider);
    assert_eq!(
        format!("{AnthropicProviderFactory:?}"),
        "AnthropicProviderFactory"
    );
    accepts_factory(Arc::new(AnthropicProviderFactory));
}

#[tokio::test]
async fn factory_preserves_pinned_identity_binding_and_base_url() {
    let pinned_server = MockServer::start().await;
    let newer_server = MockServer::start().await;
    expect_key(&pinned_server, "pinned-key", 1).await;
    let drops = Arc::new(AtomicUsize::new(0));
    let source = Arc::new(MockSource::new([lease(9, "pinned-key", &drops)]));
    let factory: Arc<dyn ProviderAdapterFactory> = Arc::new(AnthropicProviderFactory);
    let provider = factory
        .create(
            stored_provider(7, "anthropic_compatible", pinned_server.uri()),
            source.clone(),
        )
        .unwrap();

    // A later revision exists, but this already-created adapter remains pinned.
    let _newer_revision = stored_provider(8, "anthropic_compatible", newer_server.uri());
    let _stream = provider
        .complete_stream(request("anthropic"))
        .await
        .unwrap();

    assert_eq!(
        source.bindings.lock().unwrap().as_slice(),
        ["provider:anthropic:api_key"]
    );
    assert!(newer_server.received_requests().await.unwrap().is_empty());
    pinned_server.verify().await;
}

#[test]
fn factory_preflight_is_redacted_and_never_resolves_credentials() {
    let factory = AnthropicProviderFactory;
    let source = Arc::new(MockSource::new([]));
    let unsupported = AnthropicProviderFactory::validate_definition(&stored_provider(
        1,
        "secret-provider-kind",
        "https://example.invalid".into(),
    ))
    .unwrap_err();
    assert_eq!(unsupported, ProviderFactoryError::UnsupportedKind);
    assert_eq!(unsupported.to_string(), "provider kind is unsupported");
    assert!(!format!("{unsupported:?}").contains("secret-provider-kind"));

    let invalid = AnthropicProviderFactory::validate_definition(&stored_provider(
        2,
        "anthropic_compatible",
        "not a url".into(),
    ))
    .unwrap_err();
    assert_eq!(invalid, ProviderFactoryError::InvalidDefinition);
    assert_eq!(invalid.to_string(), "provider definition is invalid");
    assert!(!format!("{invalid:?}").contains("not a url"));

    let valid = stored_provider(3, "anthropic_compatible", "https://example.invalid".into());
    AnthropicProviderFactory::validate_definition(&valid).unwrap();
    factory.create(valid, source.clone()).unwrap();
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    assert!(source.bindings.lock().unwrap().is_empty());
}

#[test]
fn model_preflight_accepts_exactly_the_adapter_capability_surface() {
    let factory: &dyn ProviderAdapterFactory = &AnthropicProviderFactory;
    let provider = stored_provider(3, "anthropic_compatible", "https://example.invalid".into());
    let model = stored_model(
        "anthropic",
        &[
            "extended_thinking",
            "prompt_caching",
            "structured_output",
            "tool_use",
            "vision",
            "document_input",
        ],
    );
    factory.preflight(&provider, &model).unwrap();

    let historical = stored_model("anthropic", &["reasoning"]);
    factory.preflight(&provider, &historical).unwrap();
}

#[test]
fn model_preflight_fails_closed_with_redacted_typed_errors() {
    let factory: &dyn ProviderAdapterFactory = &AnthropicProviderFactory;
    let provider = stored_provider(3, "anthropic_compatible", "https://example.invalid".into());

    let mismatch = factory
        .preflight(&provider, &stored_model("other-provider", &["tool_use"]))
        .unwrap_err();
    assert_eq!(mismatch, ProviderFactoryError::ModelProviderMismatch);

    let unsupported = factory
        .preflight(
            &provider,
            &stored_model("anthropic", &["secret_future_capability"]),
        )
        .unwrap_err();
    assert_eq!(
        unsupported,
        ProviderFactoryError::UnsupportedModelCapability
    );
    assert!(!unsupported.to_string().contains("secret_future_capability"));
    assert!(!format!("{unsupported:?}").contains("secret_future_capability"));

    let malformed = factory
        .preflight(&provider, &stored_model("anthropic", &["TOOL_USE"]))
        .unwrap_err();
    assert_eq!(malformed, ProviderFactoryError::InvalidModelDefinition);
    assert!(!malformed.to_string().contains("TOOL_USE"));
}

#[tokio::test]
async fn router_routes_same_model_name_by_exact_provider() {
    let alpha = Arc::new(RouteProbe::named("alpha selected"));
    let beta = Arc::new(RouteProbe::named("beta selected"));
    let router = router(
        [("alpha", alpha.clone()), ("beta", beta.clone())],
        [
            ModelRef::new("alpha", "shared"),
            ModelRef::new("beta", "shared"),
        ],
    )
    .unwrap();

    let Err(alpha_error) = router
        .complete_stream(qualified_request("alpha", "shared"))
        .await
    else {
        panic!("alpha route should return its probe error");
    };
    let Err(beta_error) = router
        .complete_stream(qualified_request("beta", "shared"))
        .await
    else {
        panic!("beta route should return its probe error");
    };

    assert_eq!(alpha_error.message, "alpha selected");
    assert_eq!(beta_error.message, "beta selected");
    assert_eq!(alpha.calls.load(Ordering::SeqCst), 1);
    assert_eq!(beta.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        alpha.requests.lock().unwrap().as_slice(),
        [ModelRef::new("alpha", "shared")]
    );
    assert_eq!(
        beta.requests.lock().unwrap().as_slice(),
        [ModelRef::new("beta", "shared")]
    );
}

#[tokio::test]
async fn missing_capability_is_rejected_before_credentials_or_adapter_open() {
    let source = Arc::new(MockSource::new([]));
    let adapter: Arc<dyn ModelProvider> = Arc::new(RequestScopedAnthropicProvider::new(
        "alpha",
        1,
        "https://example.invalid",
        "secret-binding",
        source.clone(),
    ));
    let router = PinnedProviderRouter::new(
        HashMap::from([("alpha".into(), adapter)]),
        HashMap::from([(ModelRef::new("alpha", "shared"), ModelCapabilities::empty())]),
    )
    .unwrap();

    let Err(error) = router.complete_stream(tool_request("alpha")).await else {
        panic!("missing capability must be rejected");
    };

    assert_eq!(error.kind, ProviderErrorKind::Unsupported);
    assert_eq!(error.phase, ProviderErrorPhase::Open);
    assert!(!error.is_retryable());
    assert_eq!(
        error.message,
        "requested model does not support required capabilities"
    );
    assert!(!error.to_string().contains("secret-request-id"));
    assert!(!error.to_string().contains("secret-tool"));
    assert!(!error.to_string().contains("secret-binding"));
    assert_eq!(source.calls.load(Ordering::SeqCst), 0);
    assert!(source.bindings.lock().unwrap().is_empty());
}

#[tokio::test]
async fn same_model_id_capabilities_are_isolated_by_provider() {
    let alpha = Arc::new(RouteProbe::named("alpha selected"));
    let beta = Arc::new(RouteProbe::named("beta must not be called"));
    let router = router_with_capabilities(
        [("alpha", alpha.clone()), ("beta", beta.clone())],
        [
            (
                ModelRef::new("alpha", "shared"),
                ModelCapabilities::TOOL_USE,
            ),
            (ModelRef::new("beta", "shared"), ModelCapabilities::empty()),
        ],
    )
    .unwrap();

    let Err(supported) = router.complete_stream(tool_request("alpha")).await else {
        panic!("supported request must reach the selected probe");
    };
    let Err(unsupported) = router.complete_stream(tool_request("beta")).await else {
        panic!("provider-specific missing capability must be rejected");
    };

    assert_eq!(supported.message, "alpha selected");
    assert_eq!(unsupported.kind, ProviderErrorKind::Unsupported);
    assert_eq!(alpha.calls.load(Ordering::SeqCst), 1);
    assert_eq!(beta.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        alpha.requests.lock().unwrap().as_slice(),
        [ModelRef::new("alpha", "shared")]
    );
}

#[tokio::test]
async fn router_rejects_unknown_and_disallowed_models_before_adapter_calls() {
    let alpha = Arc::new(RouteProbe::named("alpha must not be called"));
    let beta = Arc::new(RouteProbe::named("beta must not be called"));
    let router = router(
        [("alpha", alpha.clone()), ("beta", beta.clone())],
        [
            ModelRef::new("alpha", "shared"),
            ModelRef::new("beta", "shared"),
        ],
    )
    .unwrap();

    let Err(unknown) = router
        .complete_stream(qualified_request("missing", "shared"))
        .await
    else {
        panic!("unknown route must be rejected");
    };
    let Err(disallowed) = router
        .complete_stream(qualified_request("alpha", "other"))
        .await
    else {
        panic!("disallowed model must be rejected");
    };

    assert_eq!(unknown, disallowed);
    assert_eq!(unknown.kind, ProviderErrorKind::InvalidRequest);
    assert_eq!(unknown.phase, ProviderErrorPhase::Open);
    assert!(!unknown.kind.is_retryable());
    assert_eq!(alpha.calls.load(Ordering::SeqCst), 0);
    assert_eq!(beta.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn router_never_falls_back_after_selected_provider_error() {
    let failure = ProviderError::new(
        ProviderErrorKind::Unavailable,
        ProviderErrorPhase::Open,
        "selected route unavailable",
    );
    let alpha = Arc::new(RouteProbe::failing(failure.clone()));
    let beta = Arc::new(RouteProbe::named("beta must not be called"));
    let router = router(
        [("alpha", alpha.clone()), ("beta", beta.clone())],
        [
            ModelRef::new("alpha", "shared"),
            ModelRef::new("beta", "shared"),
        ],
    )
    .unwrap();

    let Err(error) = router
        .complete_stream(qualified_request("alpha", "shared"))
        .await
    else {
        panic!("selected route must propagate its error");
    };

    assert_eq!(error, failure);
    assert_eq!(alpha.calls.load(Ordering::SeqCst), 1);
    assert_eq!(beta.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn router_construction_rejects_partial_catalogs_and_debug_shows_only_counts() {
    let alpha = Arc::new(RouteProbe::named("alpha"));
    let beta = Arc::new(RouteProbe::named("beta"));
    assert!(matches!(
        router(
            [("alpha", alpha.clone())],
            [
                ModelRef::new("alpha", "shared"),
                ModelRef::new("beta", "shared")
            ]
        ),
        Err(ProviderRouterBuildError::IncompleteCatalog)
    ));
    assert!(matches!(
        router(
            [("alpha", alpha.clone()), ("beta", beta.clone())],
            [ModelRef::new("alpha", "shared")]
        ),
        Err(ProviderRouterBuildError::IncompleteCatalog)
    ));

    let complete = router(
        [("alpha", alpha), ("beta", beta)],
        [
            ModelRef::new("alpha", "shared"),
            ModelRef::new("beta", "shared"),
        ],
    )
    .unwrap();
    assert_eq!(
        format!("{complete:?}"),
        "PinnedProviderRouter { route_count: 2, model_count: 2 }"
    );
}
