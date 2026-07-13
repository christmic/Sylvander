#![cfg(unix)]

//! Terminal-process verification against the real Agent runtime stack.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::json;
use sylvander_agent::prelude::{
    AgentRun, AgentSpec, InProcessMessageBus, MessageBus, MessageKind, ModelCapabilities,
    StreamEvent, SubscriptionFilter, ToolRegistry,
};
use sylvander_agent::session_store::{SessionStore, SqliteSessionStore};
use sylvander_agent::tools::AskUserTool;
use sylvander_channel::{Channel, ChannelContext};
use sylvander_channel_unix::{RuntimeInfo, UnixChannel};
use sylvander_llm_anthropic::api::client::AnthropicClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[derive(Clone, Default)]
struct RealAgentScenario {
    request_index: Arc<AtomicUsize>,
}

impl Respond for RealAgentScenario {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        match self.request_index.fetch_add(1, Ordering::SeqCst) {
            0 => ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_real_runtime", "type": "message", "role": "assistant",
                "content": [{"type": "text", "text": "Persisted through real AgentRun."}],
                "model": "sylvander-test-model", "stop_reason": "end_turn",
                "usage": {"input_tokens": 11, "output_tokens": 5}
            })),
            1 => ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_real_question", "type": "message", "role": "assistant",
                "content": [{
                    "type": "tool_use", "id": "question_real_1", "name": "ask_user",
                    "input": {
                        "question": "Which safe direction?", "options": [],
                        "multi_select": false
                    }
                }],
                "model": "sylvander-test-model", "stop_reason": "tool_use",
                "usage": {"input_tokens": 13, "output_tokens": 7}
            })),
            2 => ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_real_answer", "type": "message", "role": "assistant",
                "content": [{"type": "text", "text": "Real AskUser answer accepted."}],
                "model": "sylvander-test-model", "stop_reason": "end_turn",
                "usage": {"input_tokens": 19, "output_tokens": 5}
            })),
            3 => ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .set_body_json(json!({
                    "id": "msg_real_slow", "type": "message", "role": "assistant",
                    "content": [{"type": "text", "text": "This must not render."}],
                    "model": "sylvander-test-model", "stop_reason": "end_turn",
                    "usage": {"input_tokens": 9, "output_tokens": 4}
                })),
            index => ResponseTemplate::new(500)
                .set_body_string(format!("unexpected real Agent scenario request {index}")),
        }
    }
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

fn run_tui(socket_path: &Path, interact: impl FnOnce(&mut dyn Write, &Mutex<Vec<u8>>)) -> String {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 36,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pseudo-terminal");
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_sylvander-tui"));
    command.arg(socket_path);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    command.env("SYLVANDER_TUI_REDUCED_MOTION", "true");
    command.env("SYLVANDER_TUI_RENDER_FPS", "120");
    command.env("SYLVANDER_HISTORY_PATH", "");
    let mut child = pair.slave.spawn_command(command).expect("start TUI");
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
            Duration::from_secs(4)
        ),
        "TUI welcome did not render"
    );
    interact(&mut writer, &captured);
    writer.write_all(b"\x1b").expect("send idle escape");
    writer.flush().expect("flush idle escape");

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
    String::from_utf8_lossy(&captured.lock().expect("lock final output")).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_agent_runtime_persists_and_resumes_a_terminal_session() {
    let upstream = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(RealAgentScenario::default())
        .mount(&upstream)
        .await;

    let temp = tempfile::tempdir().expect("create runtime tempdir");
    let socket_path = temp.path().join("sylvander.sock");
    let store: Arc<dyn SessionStore> = Arc::new(
        SqliteSessionStore::open(temp.path().join("sessions.db"))
            .await
            .expect("open SQLite session store"),
    );
    let bus = Arc::new(InProcessMessageBus::new());
    let mut observed = bus
        .subscribe(SubscriptionFilter::all())
        .await
        .expect("subscribe runtime observer");
    let completed_turns = Arc::new(AtomicUsize::new(0));
    let observed_turns = Arc::clone(&completed_turns);
    let observer_task = tokio::spawn(async move {
        while let Some(message) = observed.recv().await {
            if matches!(message.kind, MessageKind::Stream(StreamEvent::Done { .. })) {
                observed_turns.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    let client = AnthropicClient::builder()
        .api_key("test-key")
        .base_url(upstream.uri())
        .build()
        .expect("build local model client");
    let spec = AgentSpec::builder()
        .id("real-runtime-test")
        .name("Sylvander")
        .model_name("sylvander-test-model")
        .build()
        .expect("build agent spec");
    let run = AgentRun::builder(spec, client)
        .bus(bus.clone())
        .session_store(store.clone())
        .override_tools(ToolRegistry::new().register(AskUserTool::new()))
        .model_capabilities(ModelCapabilities::TOOL_USE)
        .build()
        .expect("build AgentRun");
    let runtime_control = run.clone();
    let agent_id = run.id().clone();
    let inbox = bus
        .subscribe(run.subscription_filter())
        .await
        .expect("subscribe AgentRun");
    let agent_task = tokio::spawn(run.run(inbox));
    let channel = Arc::new(
        UnixChannel::new(&socket_path, agent_id)
            .with_runtime_info(RuntimeInfo {
                model: "sylvander-test-model".into(),
                reasoning_effort: sylvander_protocol::ReasoningEffort::Off,
                models: Vec::new(),
                permissions: sylvander_protocol::PermissionProfile::default(),
                capabilities: 0,
                approval_enabled: false,
                max_attachment_bytes: 512 * 1024,
                platform: sylvander_protocol::PlatformSnapshot::default(),
            })
            .with_runtime_control(runtime_control),
    );
    let channel_task = tokio::spawn(channel.run(ChannelContext {
        bus: bus.clone(),
        sessions: store,
    }));
    let socket_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !socket_path.exists() {
        assert!(
            tokio::time::Instant::now() < socket_deadline,
            "UnixChannel did not create its socket"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let completed_for_tui = Arc::clone(&completed_turns);
    let first = run_tui(&socket_path, |writer, captured| {
        writer
            .write_all(b"persist this turn\r")
            .expect("submit real Agent turn");
        writer.flush().expect("flush real Agent turn");
        if !wait_for_output(captured, "AgentRun.", Duration::from_secs(5)) {
            panic!(
                "real Agent response was not rendered; output={}",
                String::from_utf8_lossy(&captured.lock().expect("lock failed output"))
            );
        }
        writer.write_all(b"ask me\r").expect("submit AskUser turn");
        writer.flush().expect("flush AskUser turn");
        assert!(
            wait_for_output(captured, "Type your answer", Duration::from_secs(4)),
            "real Agent AskUser input was not rendered"
        );
        captured.lock().expect("clear AskUser output").clear();
        writer
            .write_all(b"use the safe path\r")
            .expect("answer real Agent AskUser");
        writer.flush().expect("flush real Agent answer");
        if !wait_for_output(captured, "Real", Duration::from_secs(4)) {
            panic!(
                "real Agent did not continue after AskUser answer; output={}",
                String::from_utf8_lossy(&captured.lock().expect("lock failed output"))
            );
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while completed_for_tui.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            completed_for_tui.load(Ordering::SeqCst),
            2,
            "real Agent AskUser turn did not publish Done"
        );
        std::thread::sleep(Duration::from_millis(150));

        captured.lock().expect("clear PTY output").clear();
        writer
            .write_all(b"interrupt the real turn\r")
            .expect("submit interruptible real Agent turn");
        writer.flush().expect("flush interruptible turn");
        if !wait_for_output(captured, "esc interrupt", Duration::from_secs(3)) {
            panic!(
                "real Agent turn did not enter interruptible state; output={}",
                String::from_utf8_lossy(&captured.lock().expect("lock failed output"))
            );
        }
        writer
            .write_all(b"\x1b")
            .expect("interrupt real Agent turn");
        writer.flush().expect("flush real Agent interrupt");
        assert!(
            wait_for_output(captured, "interrupted", Duration::from_secs(3)),
            "real Agent interrupt terminal event was not rendered"
        );
    });
    assert!(first.contains("interrupted"));

    let second = run_tui(&socket_path, |writer, captured| {
        writer.write_all(b"\x10").expect("open persisted sessions");
        writer.flush().expect("flush sessions shortcut");
        assert!(
            wait_for_output(captured, "Sessions", Duration::from_secs(3)),
            "persisted session overlay was not rendered"
        );
        std::thread::sleep(Duration::from_millis(150));
        writer
            .write_all(b"\r\r")
            .expect("focus and resume selected session");
        writer.flush().expect("flush session selection");
        if !wait_for_output(captured, "accepted.", Duration::from_secs(4)) {
            panic!(
                "persisted SQLite transcript was not restored; output={}",
                String::from_utf8_lossy(&captured.lock().expect("lock failed output"))
            );
        }
    });
    assert!(second.contains("persist") && second.contains("turn"));

    channel_task.abort();
    agent_task.abort();
    observer_task.abort();
}
