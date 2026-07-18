#![cfg(unix)]

//! Real terminal-process verification for the TUI binary.
//!
//! Unlike the transport-only E2E tests, this test owns a pseudo-terminal,
//! starts the compiled binary, sends terminal key input, resizes the terminal,
//! and verifies text emitted by ratatui after a server round trip.

use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn unique_socket_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "sylvander-tui-pty-{}-{}.sock",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos())
    ))
}

fn pty_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
                r#"{{"type":"welcome","protocol":{{"server_name":"pty-test","version":{},"capabilities":[]}}}}"#,
                sylvander_protocol::UI_PROTOCOL_VERSION
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
                    Some("discover_agents") => &[
                        r#"{"type":"agents_discovered","agents":[{"id":"pty-agent","revision":1,"name":"PTY Agent","provider_id":"test","default_model_id":"test-model","models":[],"default_prompt_profile":null,"agent_workspace":null}]}"#,
                    ],
                    Some("get_runtime_info") => &[
                        r#"{"type":"runtime_info","model":"test-model","reasoning_effort":"medium","models":[],"permissions":{"file_access":"workspace_write","network_access":"denied","approval_policy":"ask"},"capabilities":0,"approval_enabled":true,"max_attachment_bytes":1048576,"platform":{}}"#,
                    ],
                    Some("create_session") => {
                        &[r#"{"type":"session_created","session_id":"pty-session"}"#]
                    }
                    Some("chat") if message["text"] == "hello from PTY" => &[
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

fn spawn_welcome_server(path: &Path) -> std::thread::JoinHandle<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).expect("bind surface test socket");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept surface connection");
        let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone socket"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("read protocol hello");
        let hello: serde_json::Value = serde_json::from_str(&line).expect("parse protocol hello");
        assert_eq!(hello["type"], "hello");
        writeln!(
            stream,
            r#"{{"type":"welcome","protocol":{{"server_name":"surface-test","version":{},"capabilities":[]}}}}"#,
            sylvander_protocol::UI_PROTOCOL_VERSION
        )
        .expect("send welcome");
        stream.flush().expect("flush welcome");
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            line.clear();
        }
    })
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
    let mut observed = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let message = messages.recv_timeout(remaining).unwrap_or_else(|error| {
            panic!("missing client message {expected_type}: {error}; observed={observed:?}")
        });
        if message["type"] == expected_type {
            return message;
        }
        observed.push(message["type"].clone());
    }
}

#[test]
fn binary_completes_chat_decisions_interrupt_and_resize() {
    let _guard = pty_test_guard();
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
    command.arg("--socket");
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

    if !wait_for_output(
        &captured,
        "What should we work through?",
        Duration::from_secs(3),
    ) {
        let rendered = captured.lock().expect("lock startup output").clone();
        child.kill().expect("kill unresponsive TUI child");
        panic!(
            "TUI did not render its welcome surface before input; output={}",
            String::from_utf8_lossy(&rendered)
        );
    }
    std::thread::sleep(Duration::from_millis(200));
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
        wait_for_output(&captured, "Permission needed", Duration::from_secs(3)),
        "approval Decision Dock was not rendered"
    );
    writer.write_all(b"n").expect("reject approval");
    writer.flush().expect("flush rejection");
    assert!(
        wait_for_output(&captured, "Add guidance", Duration::from_secs(3)),
        "approval guidance input was not rendered"
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

#[test]
fn binary_renders_across_compact_tmux_and_ghostty_term_surfaces() {
    let _guard = pty_test_guard();
    for (term, initial) in [
        (
            "screen-256color",
            PtySize {
                rows: 18,
                cols: 40,
                pixel_width: 0,
                pixel_height: 0,
            },
        ),
        (
            "xterm-ghostty",
            PtySize {
                rows: 24,
                cols: 88,
                pixel_width: 0,
                pixel_height: 0,
            },
        ),
    ] {
        let socket_path = unique_socket_path();
        let server = spawn_welcome_server(&socket_path);
        let pair = native_pty_system()
            .openpty(initial)
            .expect("open surface PTY");
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sylvander-tui"));
        command.arg("--socket");
        command.arg(&socket_path);
        command.env("TERM", term);
        command.env("COLORTERM", "truecolor");
        command.env("SYLVANDER_TUI_COLOR", "truecolor");
        command.env_remove("NO_COLOR");
        command.env("SYLVANDER_TUI_REDUCED_MOTION", "true");
        command.env("SYLVANDER_HISTORY_PATH", "");
        let mut child = pair
            .slave
            .spawn_command(command)
            .expect("start surface TUI");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("clone PTY reader");
        let captured = Arc::new(Mutex::new(Vec::new()));
        let reader_captured = captured.clone();
        let output = std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            loop {
                let count = reader.read(&mut buffer).expect("read surface output");
                if count == 0 {
                    break;
                }
                reader_captured
                    .lock()
                    .unwrap()
                    .extend_from_slice(&buffer[..count]);
            }
        });
        let mut writer = pair.master.take_writer().expect("surface writer");
        assert!(
            wait_for_output(
                &captured,
                "What should we work through?",
                Duration::from_secs(3)
            ),
            "{term} did not render the welcome transcript"
        );
        for size in [
            PtySize {
                rows: 24,
                cols: 88,
                pixel_width: 0,
                pixel_height: 0,
            },
            PtySize {
                rows: 30,
                cols: 132,
                pixel_width: 0,
                pixel_height: 0,
            },
        ] {
            pair.master.resize(size).expect("resize surface PTY");
            std::thread::sleep(Duration::from_millis(50));
        }
        writer.write_all(b"\x1b").expect("exit surface TUI");
        writer.flush().expect("flush surface exit");
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.try_wait().expect("poll surface TUI").is_none() {
            if Instant::now() >= deadline {
                child.kill().expect("kill stuck surface TUI");
                panic!("{term} TUI did not exit");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(writer);
        output.join().expect("join surface reader");
        let rendered = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
        assert!(rendered.contains("Sylvander"));
        assert!(rendered.contains("What should we work through?"));
        if term == "xterm-ghostty" {
            let sgr_samples = rendered
                .match_indices("\u{1b}[")
                .take(32)
                .map(|(index, _)| {
                    rendered[index..]
                        .chars()
                        .take(32)
                        .collect::<String>()
                        .escape_debug()
                        .to_string()
                })
                .collect::<Vec<_>>();
            assert!(
                rendered.contains("\u{1b}[38;2;"),
                "Ghostty did not emit a true-color foreground SGR sequence: {sgr_samples:?}"
            );
        }
        server.join().expect("join surface server");
        let _ = std::fs::remove_file(socket_path);
    }
}
