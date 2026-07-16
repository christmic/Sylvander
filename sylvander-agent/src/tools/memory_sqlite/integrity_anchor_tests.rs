use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Method;
use rusqlite::TransactionBehavior;
use wiremock::matchers::path;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::integrity::database_root;
use super::{
    HttpMemoryIntegrityAnchor, HttpMemoryIntegrityAnchorConfig, MemoryAnchorError,
    MemoryIntegrityConfig, MonotonicMemoryAnchor, RelationshipMemoryRetentionPolicy,
    SqliteMemoryStore,
};
use crate::tools::memory::{MemoryAppend, MemoryExecutionContext, MemoryStore};
use sylvander_protocol::SessionContext;

const TOKEN: &[u8] = b"remote-anchor-test-token";
const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct CasState {
    value: Option<Vec<u8>>,
    version: u64,
    get_failures: u8,
    get_delay: Option<Duration>,
    force_conflict: bool,
}

#[derive(Clone)]
struct CasResponder(Arc<Mutex<CasState>>);

impl Respond for CasResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        if request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            != Some("Bearer remote-anchor-test-token")
        {
            return ResponseTemplate::new(401);
        }
        let mut state = self.0.lock().unwrap();
        if request.method == Method::GET {
            if state.get_failures > 0 {
                state.get_failures -= 1;
                return ResponseTemplate::new(503).set_body_string("do not expose this body");
            }
            let delay = state.get_delay;
            let Some(value) = state.value.clone() else {
                return ResponseTemplate::new(404);
            };
            let response = ResponseTemplate::new(200)
                .insert_header("etag", format!("\"v{}\"", state.version))
                .set_body_bytes(value);
            return delay.map_or(response.clone(), |delay| response.set_delay(delay));
        }
        if request.method != Method::PUT {
            return ResponseTemplate::new(405);
        }
        if state.force_conflict {
            return ResponseTemplate::new(412);
        }
        let current = format!("\"v{}\"", state.version);
        let create = request
            .headers
            .get("if-none-match")
            .and_then(|value| value.to_str().ok())
            == Some("*");
        let matches = request
            .headers
            .get("if-match")
            .and_then(|value| value.to_str().ok())
            == Some(current.as_str());
        if (create && state.value.is_none()) || (!create && matches && state.value.is_some()) {
            state.version += 1;
            state.value = Some(request.body.clone());
            return ResponseTemplate::new(if create { 201 } else { 200 })
                .insert_header("etag", format!("\"v{}\"", state.version));
        }
        ResponseTemplate::new(412)
    }
}

async fn server(state: Arc<Mutex<CasState>>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(path("/anchor"))
        .respond_with(CasResponder(state))
        .mount(&server)
        .await;
    server
}

fn test_anchor(endpoint: &str, timeout: Duration, retries: u8) -> HttpMemoryIntegrityAnchor {
    HttpMemoryIntegrityAnchor::new(
        HttpMemoryIntegrityAnchorConfig::new_test_http(endpoint, TOKEN, timeout, retries).unwrap(),
    )
    .unwrap()
}

fn worker() -> MemoryExecutionContext {
    MemoryExecutionContext::application_worker(&SessionContext::new(
        "alice",
        "agent-a",
        "session-a",
    ))
}

fn open_store(
    database: &std::path::Path,
    endpoint: &str,
) -> Result<SqliteMemoryStore, crate::tools::memory::MemoryStoreError> {
    let anchor = test_anchor(endpoint, Duration::from_secs(2), 1);
    SqliteMemoryStore::open_with_integrity(
        database,
        RelationshipMemoryRetentionPolicy::default(),
        MemoryIntegrityConfig::with_anchor(Arc::new(anchor), KEY)?,
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn http_backend_uses_authenticated_create_load_and_cas() {
    let state = Arc::new(Mutex::new(CasState::default()));
    let server = server(Arc::clone(&state)).await;
    let endpoint = format!("{}/anchor", server.uri());
    tokio::task::spawn_blocking(move || {
        let anchor = test_anchor(&endpoint, Duration::from_secs(1), 0);
        assert!(anchor.load().unwrap().is_none());
        let first = anchor.create(b"first").unwrap();
        assert_eq!(anchor.load().unwrap().unwrap().value, b"first");
        let second = anchor.compare_and_swap(&first, b"second").unwrap();
        assert_eq!(anchor.load().unwrap().unwrap().value, b"second");
        assert_eq!(
            anchor.compare_and_swap(&first, b"stale"),
            Err(MemoryAnchorError::Conflict)
        );
        assert_ne!(first, second);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn reads_retry_only_within_the_configured_bound() {
    let state = Arc::new(Mutex::new(CasState {
        value: Some(b"value".to_vec()),
        version: 7,
        get_failures: 1,
        ..CasState::default()
    }));
    let server = server(Arc::clone(&state)).await;
    let endpoint = format!("{}/anchor", server.uri());
    tokio::task::spawn_blocking(move || {
        let anchor = test_anchor(&endpoint, Duration::from_secs(1), 1);
        assert_eq!(anchor.load().unwrap().unwrap().value, b"value");
    })
    .await
    .unwrap();
    assert_eq!(state.lock().unwrap().get_failures, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_and_service_errors_fail_closed_without_response_content() {
    let state = Arc::new(Mutex::new(CasState {
        value: Some(b"value".to_vec()),
        version: 1,
        get_failures: 1,
        ..CasState::default()
    }));
    let server = server(Arc::clone(&state)).await;
    let endpoint = format!("{}/anchor", server.uri());
    let error = tokio::task::spawn_blocking(move || {
        test_anchor(&endpoint, Duration::from_secs(1), 0).load()
    })
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error, MemoryAnchorError::Unavailable);
    assert!(!error.to_string().contains("expose"));

    state.lock().unwrap().get_delay = Some(Duration::from_millis(250));
    let endpoint = format!("{}/anchor", server.uri());
    let error = tokio::task::spawn_blocking(move || {
        test_anchor(&endpoint, Duration::from_millis(100), 0).load()
    })
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error, MemoryAnchorError::Unavailable);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let disconnected = format!("http://{}/anchor", listener.local_addr().unwrap());
    drop(listener);
    let error = tokio::task::spawn_blocking(move || {
        test_anchor(&disconnected, Duration::from_millis(100), 0).load()
    })
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error, MemoryAnchorError::Unavailable);
}

#[test]
fn production_configuration_rejects_plain_http_and_secret_debugging() {
    let error = HttpMemoryIntegrityAnchorConfig::new(
        "http://anchor.example/v1/memory",
        TOKEN,
        Duration::from_secs(1),
        0,
    )
    .unwrap_err();
    assert_eq!(error, MemoryAnchorError::InvalidResponse);
    let config = HttpMemoryIntegrityAnchorConfig::new(
        "https://anchor.example/v1/memory",
        TOKEN,
        Duration::from_secs(1),
        0,
    )
    .unwrap();
    let debug = format!("{config:?}");
    assert!(!debug.contains("remote-anchor-test-token"));
    assert!(!debug.contains("anchor.example"));
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_anchor_rejects_host_replay_of_database_and_local_key() {
    let state = Arc::new(Mutex::new(CasState::default()));
    let server = server(state).await;
    let endpoint = format!("{}/anchor", server.uri());
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("memory.db");
    tokio::task::spawn_blocking({
        let database = database.clone();
        let endpoint = endpoint.clone();
        move || {
            let store = open_store(&database, &endpoint).unwrap();
            store
                .maintenance()
                .activate_staged_retention_policy()
                .unwrap();
            futures::executor::block_on(
                store.append_relationship(&worker(), MemoryAppend::new("first")),
            )
            .unwrap();
        }
    })
    .await
    .unwrap();
    let before = std::fs::read(&database).unwrap();
    tokio::task::spawn_blocking({
        let database = database.clone();
        let endpoint = endpoint.clone();
        move || {
            let store = open_store(&database, &endpoint).unwrap();
            futures::executor::block_on(
                store.append_relationship(&worker(), MemoryAppend::new("second")),
            )
            .unwrap();
        }
    })
    .await
    .unwrap();
    std::fs::write(&database, before).unwrap();
    let error = tokio::task::spawn_blocking(move || open_store(&database, &endpoint))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "store error: memory integrity verification failed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pending_anchor_recovers_only_from_the_before_or_after_root() {
    for commit_database in [false, true] {
        let state = Arc::new(Mutex::new(CasState::default()));
        let server = server(state).await;
        let endpoint = format!("{}/anchor", server.uri());
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("memory.db");
        tokio::task::spawn_blocking({
            let database = database.clone();
            let endpoint = endpoint.clone();
            move || {
                let store = open_store(&database, &endpoint).unwrap();
                store
                    .maintenance()
                    .activate_staged_retention_policy()
                    .unwrap();
                let integrity = store.integrity.as_ref().unwrap();
                let mut connection = store.connection.lock().unwrap();
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                let before = integrity.verify(&transaction).unwrap();
                transaction
                    .execute(
                        "UPDATE relationship_memory_retention_state SET clock_watermark = clock_watermark + 1 WHERE singleton = 1",
                        [],
                    )
                    .unwrap();
                let after = database_root(&transaction).unwrap();
                integrity.prepare(&before, &after).unwrap();
                if commit_database {
                    transaction.commit().unwrap();
                } else {
                    drop(transaction);
                }
            }
        })
        .await
        .unwrap();
        tokio::task::spawn_blocking(move || open_store(&database, &endpoint).unwrap())
            .await
            .unwrap();
    }
}
