#![cfg(unix)]

//! Real terminal-process verification for the TUI binary.
//!
//! Unlike the transport-only E2E tests, this test owns a pseudo-terminal,
//! starts the compiled binary, sends terminal key input, resizes the terminal,
//! and verifies text emitted by ratatui after a server round trip.

use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn unique_socket_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "sylvander-tui-pty-{}-{}.sock",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ))
}

fn spawn_server(
    path: &Path,
) -> (
    std::thread::JoinHandle<()>,
    mpsc::Receiver<serde_json::Value>,
) {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).expect("bind PTY test socket");
    let (message_tx, message_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        for connection_index in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept TUI connection");
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone socket"));
            let mut line = String::new();
            reader.read_line(&mut line).expect("read protocol hello");
            let hello: serde_json::Value =
                serde_json::from_str(&line).expect("parse protocol hello");
            assert_eq!(hello["type"], "hello");
            writeln!(
                stream,
                r#"{{"type":"welcome","protocol":{{"server_name":"pty-test","version":1,"capabilities":[]}}}}"#
            )
            .expect("send welcome");
            stream.flush().expect("flush welcome");

            loop {
                line.clear();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let message: serde_json::Value =
                    serde_json::from_str(&line).expect("parse client msg");
                message_tx
                    .send(message.clone())
                    .expect("report client message");
                let responses: &[&str] = match message["type"].as_str() {
                    Some("chat") if message["text"] == "hello from PTY" => &[
                        r#"{"type":"session_created","session_id":"pty-session"}"#,
                        r#"{"type":"text_delta","session_id":"pty-session","delta":"PTY response rendered"}"#,
                        r#"{"type":"done","session_id":"pty-session","text":"PTY response rendered"}"#,
                        r#"{"type":"approval_request","session_id":"pty-session","batch_id":"pty-approval","tools":[{"call_id":"pty-tool","tool_name":"bash","input":{"command":"rm -rf build"}}],"allowed_scopes":["once"]}"#,
                    ],
                    Some("approve") => &[
                        r#"{"type":"ask_user","session_id":"pty-session","call_id":"pty-question","question":"Which safe direction?","options":[],"multi_select":false}"#,
                    ],
                    Some("reattach_session") => &[
                        r#"{"type":"session_history","session":{"id":"pty-session","label":"PTY recovered","workspace":"/workspace/pty","last_seen_secs":0},"messages":[{"role":"user","text":"hello from PTY"},{"role":"assistant","text":"PTY response rendered"}],"iterations":1,"input_tokens":3,"output_tokens":3,"recovery":true,"replay_truncated":false,"notice":"PTY session recovered"}"#,
                    ],
                    Some("chat") if message["text"] == "interrupt me" => &[
                        r#"{"type":"text_delta","session_id":"pty-session","delta":"still working"}"#,
                    ],
                    Some("interrupt") => &[
                        r#"{"type":"turn_interrupted","session_id":"pty-session","reason":"PTY interrupt complete"}"#,
                    ],
                    _ => &[],
                };
                for response in responses {
                    writeln!(stream, "{response}").expect("send server event");
                    if response.contains(r#""type":"done""#) {
                        stream.flush().expect("flush completed turn");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                stream.flush().expect("flush server events");
                if connection_index == 0 && message["type"] == "answer" {
                    stream
                        .shutdown(std::net::Shutdown::Both)
                        .expect("disconnect first PTY service connection");
                    break;
                }
            }
        }
    });
    (server, message_rx)
}

fn wait_for_output(captured: &Mutex<Vec<u8>>, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if String::from_utf8_lossy(&captured.lock().expect("lock PTY output")).contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

fn recv_message(
    messages: &mpsc::Receiver<serde_json::Value>,
    expected_type: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let message = messages
            .recv_timeout(remaining)
            .unwrap_or_else(|error| panic!("missing client message {expected_type}: {error}"));
        if message["type"] == expected_type {
            return message;
        }
    }
}

#[test]
fn binary_completes_chat_decisions_interrupt_and_resize() {
    let socket_path = unique_socket_path();
    let (server, messages) = spawn_server(&socket_path);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 36,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pseudo-terminal");

    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sylvander-tui"));
    command.arg(&socket_path);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("SYLVANDER_TUI_REDUCED_MOTION", "true");
    command.env("SYLVANDER_TUI_RENDER_FPS", "120");
    command.env("SYLVANDER_TUI_RECONNECT_MS", "250");
    command.env("SYLVANDER_HISTORY_PATH", "");
    let mut child = pair
        .slave
        .spawn_command(command)
        .expect("start TUI in pseudo-terminal");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let reader_captured = Arc::clone(&captured);
    let output = std::thread::spawn(move || {
        let mut buffer = [0; 8 * 1024];
        loop {
            let count = reader.read(&mut buffer).expect("read TUI output");
            if count == 0 {
                break;
            }
            reader_captured
                .lock()
                .expect("lock captured output")
                .extend_from_slice(&buffer[..count]);
        }
    });
    let mut writer = pair.master.take_writer().expect("take PTY writer");

    assert!(
        wait_for_output(
            &captured,
            "What should we work through?",
            Duration::from_secs(3)
        ),
        "TUI did not render its welcome surface before input"
    );
    writer.write_all(b"hello from PTY\r").expect("type chat");
    writer.flush().expect("flush chat input");
    let submitted = recv_message(&messages, "chat");
    assert_eq!(submitted["text"], "hello from PTY");
    if !wait_for_output(&captured, "rendered", Duration::from_secs(3)) {
        let rendered = captured.lock().expect("lock failure output").clone();
        child.kill().expect("kill unresponsive TUI child");
        panic!(
            "TUI did not render the streamed response; output={}",
            String::from_utf8_lossy(&rendered)
        );
    }

    assert!(
        wait_for_output(&captured, "Tool Approval", Duration::from_secs(3)),
        "approval modal was not rendered"
    );
    writer.write_all(b"n").expect("reject approval");
    writer.flush().expect("flush rejection");
    assert!(
        wait_for_output(&captured, "Optional reason", Duration::from_secs(3)),
        "approval reason input was not rendered"
    );
    writer
        .write_all(b"unsafe location\r")
        .expect("type rejection reason");
    writer.flush().expect("flush rejection reason");
    let approval = recv_message(&messages, "approve");
    assert_eq!(approval["call_id"], "pty-tool");
    assert_eq!(approval["approved"], false);
    assert_eq!(approval["reason"], "unsafe location");

    assert!(
        wait_for_output(&captured, "Type your answer", Duration::from_secs(3)),
        "AskUser free-text input was not rendered"
    );
    writer.write_all(b"use tests\r").expect("answer AskUser");
    writer.flush().expect("flush AskUser answer");
    let answer = recv_message(&messages, "answer");
    assert_eq!(answer["call_id"], "pty-question");
    assert_eq!(answer["answer"], "use tests");

    let reattach = recv_message(&messages, "reattach_session");
    assert_eq!(reattach["session_id"], "pty-session");
    if !wait_for_output(&captured, "session recover", Duration::from_secs(3)) {
        let rendered = captured.lock().expect("lock failure output").clone();
        child.kill().expect("kill unresponsive TUI child");
        panic!(
            "reattached session history was not rendered; output={}",
            String::from_utf8_lossy(&rendered)
        );
    }

    writer
        .write_all(b"interrupt me\r")
        .expect("start interruptible turn");
    writer.flush().expect("flush interruptible turn");
    let second_chat = recv_message(&messages, "chat");
    assert_eq!(second_chat["text"], "interrupt me");
    assert!(
        wait_for_output(&captured, "still", Duration::from_secs(3)),
        "partial turn was not rendered before interrupt"
    );
    writer.write_all(b"\x1b").expect("interrupt active turn");
    writer.flush().expect("flush interrupt");
    let interrupt = recv_message(&messages, "interrupt");
    assert_eq!(interrupt["session_id"], "pty-session");
    assert!(
        wait_for_output(&captured, "complete", Duration::from_secs(3)),
        "terminal interrupt event was not rendered"
    );

    pair.master
        .resize(PtySize {
            rows: 28,
            cols: 92,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize pseudo-terminal");
    std::thread::sleep(Duration::from_millis(250));
    writer.write_all(b"\x1b").expect("send idle escape");
    writer.flush().expect("flush escape");

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait().expect("poll TUI child").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill stuck TUI child");
            panic!("TUI did not exit after idle Escape");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(writer);

    output.join().expect("join PTY reader");
    let output = captured.lock().expect("lock final output").clone();
    let rendered = String::from_utf8_lossy(&output);
    assert!(rendered.contains("Sylvander"), "brand was not rendered");
    assert!(
        rendered.contains("hello"),
        "submitted user turn was not rendered"
    );
    assert!(
        rendered.contains("response") && rendered.contains("rendered"),
        "agent response was not rendered"
    );

    server.join().expect("join PTY server");
    let _ = std::fs::remove_file(socket_path);
}
