use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use axum::{Router, http::StatusCode, routing::post};

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
    let client = Client::new("key", "secret");
    client
        .token_cache
        .lock()
        .await
        .replace(("token".into(), i64::MAX));

    client
        .reply_text(&format!("http://{address}/webhook"), "hello")
        .await;

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    server.abort();
}
