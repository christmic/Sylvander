use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

struct FakeExternalProvider {
    next_lease_generation: AtomicU64,
    acquire_count: AtomicUsize,
    renew_count: AtomicUsize,
    fail_renew: AtomicBool,
}

impl FakeExternalProvider {
    fn new() -> Self {
        Self {
            next_lease_generation: AtomicU64::new(1),
            acquire_count: AtomicUsize::new(0),
            renew_count: AtomicUsize::new(0),
            fail_renew: AtomicBool::new(false),
        }
    }

    fn issue(
        &self,
        credential_generation: u64,
        now_unix_secs: i64,
        value: &str,
    ) -> Result<ExternalSecretLease, ExternalSecretLeaseError> {
        ExternalSecretLease::new(
            credential_generation,
            self.next_lease_generation.fetch_add(1, Ordering::SeqCst),
            now_unix_secs,
            now_unix_secs + 30,
            value.as_bytes().to_vec(),
        )
    }
}

impl RenewableExternalSecretProvider for FakeExternalProvider {
    fn acquire<'a>(
        &'a self,
        _reference: &'a config::SecretRef,
        credential_generation: u64,
        now_unix_secs: i64,
    ) -> ExternalSecretLeaseFuture<'a> {
        Box::pin(async move {
            self.acquire_count.fetch_add(1, Ordering::SeqCst);
            self.issue(credential_generation, now_unix_secs, "external-key-one")
        })
    }

    fn renew<'a>(
        &'a self,
        _reference: &'a config::SecretRef,
        current: SecretLeaseMetadata,
        now_unix_secs: i64,
    ) -> ExternalSecretLeaseFuture<'a> {
        Box::pin(async move {
            self.renew_count.fetch_add(1, Ordering::SeqCst);
            if self.fail_renew.load(Ordering::SeqCst) {
                return Err(ExternalSecretLeaseError::Unavailable);
            }
            self.issue(
                current.credential_generation,
                now_unix_secs,
                "external-key-two",
            )
        })
    }
}

fn runtime_config(
    directory: &tempfile::TempDir,
    model_server: &MockServer,
) -> config::ServerConfig {
    let provider_key = directory.path().join("provider.key");
    std::fs::write(&provider_key, "0123456789abcdef0123456789abcdef").unwrap();
    let anchor_dir = directory.path().join("integrity-anchor");
    std::fs::create_dir_all(&anchor_dir).unwrap();
    config::ServerConfig::from_toml(&format!(
        r#"
schema_version = 1
[server]
data_dir = "{}"

[server.memory_maintenance.integrity]
[server.memory_maintenance.integrity.key]
source = "file"
path = "{}"
[server.memory_maintenance.integrity.backend]
kind = "file"
anchor_path = "{}"

[[model_providers]]
id = "primary"
base_url = "{}"
[model_providers.api_key]
source = "file"
path = "{}"
[[model_providers.models]]
id = "model-a"
capabilities = ["tool_use"]

[[agents]]
[agents.spec]
id = "assistant"
name = "Assistant"
[agents.spec.model]
provider = "primary"
model_name = "model-a"
allowed_models = [{{ provider_id = "primary", model_id = "model-a" }}]
"#,
        directory.path().join("data").display(),
        provider_key.display(),
        anchor_dir.join("anchor.json").display(),
        model_server.uri(),
        provider_key.display(),
    ))
    .unwrap()
}

#[tokio::test]
async fn runtime_boot_uses_external_acquire_renew_rotation_and_failure() {
    let model_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "message-external-lease",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "model": "model-a",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })))
        .expect(2)
        .mount(&model_server)
        .await;

    let directory = tempfile::tempdir().unwrap();
    let provider = Arc::new(FakeExternalProvider::new());
    let runtime = Runtime::boot_config_with_provider_credentials(
        runtime_config(&directory, &model_server),
        ProviderCredentialSources::new(Arc::new(config::SystemSecretResolver), provider.clone()),
    )
    .await
    .unwrap();
    let configured = runtime
        .revision_provider
        .as_ref()
        .unwrap()
        .active_agent(&sylvander_protocol::AgentId::new("assistant"))
        .await
        .unwrap();
    let session = sylvander_protocol::SessionId::new("external-lease-session");
    configured
        .attach_authenticated_session(
            session.clone(),
            sylvander_agent::session::SessionMetadata {
                workspace: directory.path().to_path_buf(),
                name: "external lease".into(),
                user_id: "user-1".into(),
            },
        )
        .await
        .unwrap();
    let run = configured.run.clone();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while run.get_session(&session).await.is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Agent did not observe the Runtime join event");

    run.handle_message(sylvander_agent::bus::BusMessage::user_chat(
        session.clone(),
        "user-1",
        "first",
    ))
    .await
    .unwrap();
    run.handle_message(sylvander_agent::bus::BusMessage::user_chat(
        session.clone(),
        "user-1",
        "second",
    ))
    .await
    .unwrap();
    let requests = model_server.received_requests().await.unwrap();
    let keys = requests
        .iter()
        .map(|request| {
            request
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "Bearer external-key-one".to_owned(),
            "Bearer external-key-two".to_owned()
        ]
    );

    provider.fail_renew.store(true, Ordering::SeqCst);
    assert!(
        run.handle_message(sylvander_agent::bus::BusMessage::user_chat(
            session,
            "user-1",
            "must fail closed",
        ))
        .await
        .is_err()
    );
    assert_eq!(provider.acquire_count.load(Ordering::SeqCst), 1);
    assert_eq!(provider.renew_count.load(Ordering::SeqCst), 2);

    runtime.shutdown().await.unwrap();
}
