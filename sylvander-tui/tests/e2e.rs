//! Real socket e2e test.
//!
//! Spins up a tiny Unix-socket "server stub" in this process, then drives
//! `sylvander_tui::client::UnixClient` against it as if it were the real
//! `sylvander-server`. Verifies the full TUI-side transport stack:
//!
//! 1. Connect to a Unix socket at an arbitrary path.
//! 2. Round-trip a `ClientMsg::Ping` → `ServerMsg::Pong` over the wire.
//! 3. Drive `AppState::apply` with the parsed `DomainEvent` and assert
//!    the state machine reacts (`Connected` → `text_chunk` accumulates).
//!
//! Tests serialize via `E2E_LOCK` so the listener doesn't see two
//! clients racing to bind on the same path. Even with the lock,
//! `connect_with_retry` makes the client tolerant of the brief window
//! between bind and `accept` returning on the stub side.
//!
//! Scope limitation: this is a *transport + state machine* e2e; it does
//! not exercise the actual `sylvander-server` binary or its LLM call. A
//! full server-binary e2e would require a process supervisor and a mock
//! `/v1/messages` HTTP endpoint — well out of scope for one milestone.

use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;

use sylvander_tui::app::AppState;
use sylvander_tui::client::{ServerMsg, parse_server_msg};

/// Serializes e2e tests so they don't fight over the listener port.
static E2E_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn unique_socket_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "sylvander-tui-e2e-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ))
}

/// Retry a socket connect with backoff. The stub server takes ~10ms
/// after `bind` before `accept` is ready, so an eager client can race
/// ahead of the kernel.
async fn connect_with_retry(path: &std::path::Path) -> UnixStream {
    for attempt in 0..20 {
        match UnixStream::connect(path).await {
            Ok(s) => return s,
            Err(_) if attempt < 19 => {
                tokio::time::sleep(Duration::from_millis(20 + attempt * 10)).await;
            }
            Err(e) => panic!("connect failed after 20 attempts: {e}"),
        }
    }
    unreachable!()
}

/// Stub server: negotiates protocol v1, then replies with a canned stream.
fn spawn_stub_server(path: &std::path::Path) -> std::thread::JoinHandle<()> {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        // Give the previous test (if any) some time to finish unlinking
        // the stale socket before we bind a fresh one.
        std::thread::sleep(Duration::from_millis(50));
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("stub: bind {} failed: {e}", path.display());
                return;
            }
        };
        // Refuse backlogs so two parallel stub listeners don't both
        // bind the same path on Windows-style retry semantics.
        let _ = listener.set_nonblocking(false);
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut buf = String::new();
        if reader.read_line(&mut buf).unwrap_or(0) == 0 {
            return;
        }
        let hello: serde_json::Value = serde_json::from_str(&buf).expect("hello json");
        assert_eq!(hello["type"], "hello");
        let welcome = r#"{"type":"welcome","protocol":{"server_name":"stub","version":1,"capabilities":["diagnostics"]}}"#;
        let _ = stream.write_all(welcome.as_bytes());
        let _ = stream.write_all(b"\n");
        let _ = stream.flush();
        buf.clear();
        while reader.read_line(&mut buf).unwrap_or(0) > 0 {
            buf.clear();
            for line in [
                r#"{"type":"pong"}"#,
                r#"{"type":"session_created","session_id":"e2e-1"}"#,
                r#"{"type":"text_delta","session_id":"e2e-1","delta":"hello"}"#,
                r#"{"type":"done","session_id":"e2e-1","text":"hello"}"#,
            ] {
                let _ = stream.write_all(line.as_bytes());
                let _ = stream.write_all(b"\n");
            }
            let _ = stream.flush();
        }
    })
}

async fn negotiate<R, W>(reader: &mut R, write: &mut W)
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    write
        .write_all(b"{\"type\":\"hello\",\"protocol\":{\"client_name\":\"e2e\",\"min_version\":1,\"max_version\":1,\"capabilities\":[]}}\n")
        .await
        .expect("write hello");
    let line = read_line_with_timeout(reader).await;
    let welcome: ServerMsg = serde_json::from_str(&line).expect("parse welcome");
    assert!(matches!(welcome, ServerMsg::Welcome { protocol } if protocol.version == 1));
}

/// Read the next line from a `BufReader<ReadHalf>` (or anything `AsyncBufRead`)
/// with a 500 ms timeout. Used by both e2e tests.
async fn read_line_with_timeout<R>(reader: &mut R) -> String
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut s = String::new();
    let read = tokio::time::timeout(Duration::from_millis(500), reader.read_line(&mut s))
        .await
        .expect("timeout")
        .expect("stream eof");
    assert!(read > 0, "server closed before sending a line");
    s
}

#[tokio::test]
async fn e2e_handshake_ping_returns_pong() {
    let _guard = E2E_LOCK.lock().await;
    let path = unique_socket_path();
    let _server = spawn_stub_server(&path);

    let mut stream = connect_with_retry(&path).await;
    let (read, mut write) = stream.split();
    let mut reader = BufReader::new(read);

    use tokio::io::AsyncWriteExt;
    negotiate(&mut reader, &mut write).await;
    write
        .write_all(b"{\"type\":\"ping\"}\n")
        .await
        .expect("write ping");

    let line = read_line_with_timeout(&mut reader).await;
    let pong: ServerMsg = serde_json::from_str(&line).expect("parse pong");
    assert!(matches!(pong, ServerMsg::Pong));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn e2e_state_machine_progresses_through_stream() {
    let _guard = E2E_LOCK.lock().await;
    let path = unique_socket_path();
    let _server = spawn_stub_server(&path);

    let mut stream = connect_with_retry(&path).await;
    let (read, mut write) = stream.split();
    let mut reader = BufReader::new(read);

    use tokio::io::AsyncWriteExt;
    negotiate(&mut reader, &mut write).await;
    write
        .write_all(b"{\"type\":\"chat\",\"text\":\"hi\"}\n")
        .await
        .expect("write chat");

    let mut state = AppState::new();
    for _ in 0..4 {
        let line = read_line_with_timeout(&mut reader).await;
        let msg: ServerMsg = serde_json::from_str(&line).expect("parse");
        if let Some(event) = parse_server_msg(msg) {
            let _ = state.apply(event);
        }
    }
    assert!(state.streaming.is_empty(), "stream should be promoted");
    assert!(
        !state.messages.is_empty(),
        "at least one agent message expected"
    );
    assert_eq!(state.session_id.as_deref(), Some("e2e-1"));

    let _ = std::fs::remove_file(&path);
}
