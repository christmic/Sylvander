use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sylvander_llm_core::{
    ChatMessage, ModelProvider, ModelRef, ModelRequest, ProviderErrorKind, ProviderErrorPhase,
};
use tempfile::tempdir;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::config::{SecretRef, SystemSecretResolver};
use crate::registry_domain::CredentialBindingRevision;

const TEXT_STREAM: &str = "\
event: message_start
data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-test\",\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}

event: message_delta
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":0}}

event: message_stop
data: {\"type\":\"message_stop\"}

";

fn request(provider: &str) -> ModelRequest {
    ModelRequest {
        request_id: "request-1".into(),
        model: ModelRef::new(provider, "claude-test"),
        system: Vec::new(),
        messages: vec![ChatMessage::user("hello")],
        tools: Vec::new(),
        max_output_tokens: 16,
        reasoning: None,
        output_schema: None,
    }
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
    results: Mutex<VecDeque<MockResult>>,
}

impl MockSource {
    fn new(results: impl IntoIterator<Item = MockResult>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            results: Mutex::new(results.into_iter().collect()),
        }
    }
}

impl ActiveCredentialSource for MockSource {
    fn resolve_active<'a>(&'a self, _binding_id: &'a str) -> CredentialLeaseFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
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
}
