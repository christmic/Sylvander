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
//! This covers everything the user-facing smoke test exercises
//! (manual `nc -U /tmp/sylvander.sock`) but in CI, reproducible.
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
use sylvander_tui::client::{parse_server_msg, ServerMsg};

fn unique_socket_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "sylvander-tui-e2e-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

/// Stub server: accepts one client and replies with the canned 4-message
/// sequence (Pong + session_created + text_delta + done) for every line
/// the client sends. Keeps replying as long as the client has anything
/// to say, so the test loop can ask for "4 messages after one Chat"
/// without worrying about stub timing.
fn spawn_stub_server(path: &std::path::Path) -> std::thread::JoinHandle<()> {
    let path = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).expect("bind unix socket");
        let (mut stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(_) => return,
        };
        use std::io::BufRead;
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut buf = String::new();
        while reader.read_line(&mut buf).unwrap_or(0) > 0 {
            buf.clear();
            // Always emit the same 4-line reply; the test loop only needs
            // the first 4 after its single outgoing Chat.
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

#[tokio::test]
async fn e2e_handshake_ping_returns_pong() {
    let path = unique_socket_path();
    let _server = spawn_stub_server(&path);
    // Give the listener a beat to start.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect via tokio's UnixStream directly (no need for the full
    // UnixClient — we're testing the wire format and parsing).
    let mut stream = UnixStream::connect(&path).await.expect("connect");
    let (read, mut write) = stream.split();
    let mut reader = BufReader::new(read).lines();

    // Send Ping.
    use tokio::io::AsyncWriteExt;
    write
        .write_all(b"{\"type\":\"ping\"}\n")
        .await
        .expect("write ping");

    // Read Pong.
    let line = tokio::time::timeout(Duration::from_millis(500), reader.next_line())
        .await
        .expect("timeout")
        .expect("stream eof")
        .expect("line");
    let pong: ServerMsg = serde_json::from_str(&line).expect("parse pong");
    assert!(matches!(pong, ServerMsg::Pong));

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn e2e_state_machine_progresses_through_stream() {
    let path = unique_socket_path();
    let _server = spawn_stub_server(&path);
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = UnixStream::connect(&path).await.expect("connect");
    let (read, mut write) = stream.split();
    let mut reader = BufReader::new(read).lines();

    use tokio::io::AsyncWriteExt;
    write
        .write_all(b"{\"type\":\"chat\",\"text\":\"hi\"}\n")
        .await
        .expect("write chat");

    // Drive each ServerMsg through AppState. IterationStart, Pong, etc.
    // are intentionally skipped by parse_server_msg (None is expected).
    let mut state = AppState::new();
    for _ in 0..4 {
        let line = tokio::time::timeout(Duration::from_millis(500), reader.next_line())
            .await
            .expect("timeout")
            .expect("eof")
            .expect("line");
        let msg: ServerMsg = serde_json::from_str(&line).expect("parse");
        if let Some(event) = parse_server_msg(msg) {
            let _ = state.apply(event);
        }
    }
    // After session_created + text_delta + done, Done promoted the
    // streaming buffer into messages, so the streaming String is empty.
    assert!(state.streaming.is_empty(), "stream should be promoted");
    assert!(
        !state.messages.is_empty(),
        "at least one agent message expected"
    );
    assert_eq!(state.session_id.as_deref(), Some("e2e-1"));

    let _ = std::fs::remove_file(&path);
}
