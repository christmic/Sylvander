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

fn spawn_server(path: &Path) -> (std::thread::JoinHandle<()>, mpsc::Receiver<String>) {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).expect("bind PTY test socket");
    let (chat_tx, chat_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept TUI connection");
        let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone socket"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("read protocol hello");
        let hello: serde_json::Value = serde_json::from_str(&line).expect("parse protocol hello");
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
            let message: serde_json::Value = serde_json::from_str(&line).expect("parse client msg");
            if message["type"] != "chat" {
                continue;
            }
            let text = message["text"].as_str().unwrap_or_default().to_owned();
            chat_tx.send(text).expect("report submitted chat");
            for response in [
                r#"{"type":"session_created","session_id":"pty-session"}"#,
                r#"{"type":"text_delta","session_id":"pty-session","delta":"PTY response rendered"}"#,
                r#"{"type":"done","session_id":"pty-session","text":"PTY response rendered"}"#,
            ] {
                writeln!(stream, "{response}").expect("send server event");
            }
            stream.flush().expect("flush server events");
        }
    });
    (server, chat_rx)
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

#[test]
fn binary_renders_chat_and_survives_terminal_resize() {
    let socket_path = unique_socket_path();
    let (server, chat_rx) = spawn_server(&socket_path);
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
    let submitted = chat_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap_or_else(|error| {
            let rendered = captured.lock().expect("lock failure output").clone();
            child.kill().expect("kill unresponsive TUI child");
            panic!(
                "TUI did not submit chat: {error}; output={}",
                String::from_utf8_lossy(&rendered)
            );
        });
    assert_eq!(submitted, "hello from PTY");
    if !wait_for_output(&captured, "rendered", Duration::from_secs(3)) {
        let rendered = captured.lock().expect("lock failure output").clone();
        child.kill().expect("kill unresponsive TUI child");
        panic!(
            "TUI did not render the streamed response; output={}",
            String::from_utf8_lossy(&rendered)
        );
    }

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
