use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, http::StatusCode, routing::post};
use sylvander_channel::credential::{
    CredentialLeaseBundle, CredentialLeaseError, CredentialLeaseRequest, CredentialLeaseSource,
};

struct StaticCredentials;

#[async_trait::async_trait]
impl CredentialLeaseSource for StaticCredentials {
    async fn lease(
        &self,
        request: &CredentialLeaseRequest,
    ) -> Result<CredentialLeaseBundle, CredentialLeaseError> {
        let now = unix_timestamp();
        CredentialLeaseBundle::new(
            1,
            1,
            now,
            now + 30,
            request.slots.iter().map(|slot| {
                let value = if slot == "app_key" { "key" } else { "secret" };
                (slot.clone(), value.as_bytes().to_vec())
            }),
        )
    }
}

#[tokio::test]
async fn webhook_delivery_retries_retryable_status() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let state = attempts.clone();
    let app = Router::new().route(
        "/webhook",
        post(move || {
            let state = state.clone();
            async move {
                if state.fetch_add(1, Ordering::SeqCst) == 0 {
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    StatusCode::OK
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = Client::new("bot-a", Arc::new(StaticCredentials)).unwrap();
    client
        .token_cache
        .lock()
        .await
        .replace(("token".into(), i64::MAX, 1));

    client
        .reply_text(&format!("http://{address}/webhook"), "hello")
        .await;

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn access_token_cache_is_bound_to_credential_generation() {
    let client = Client::new("bot-a", Arc::new(StaticCredentials)).unwrap();
    client
        .token_cache
        .lock()
        .await
        .replace(("token".into(), i64::MAX, 7));

    assert_eq!(
        client.cached_access_token(7, unix_timestamp()).await,
        Some("token".into())
    );
    assert_eq!(client.cached_access_token(8, unix_timestamp()).await, None);
}
