//! Sylvander server — boots the agent system with channels.
//!
//! # Env vars
//!
//! ```text
//! ANTHROPIC_API_KEY      (required) Anthropic API key
//! ANTHROPIC_BASE_URL     (optional) default: https://api.anthropic.com
//! SYLVANDER_MODEL        (optional) default: claude-sonnet-5-20260601
//! DINGTALK_APP_KEY       (optional) enable DingTalk channel
//! DINGTALK_APP_SECRET    (optional)
//! SYLVANDER_APPROVAL     (optional) enable bus-based tool approval
//! ```

use std::sync::Arc;

use sylvander_agent::bus::{InProcessMessageBus, MessageBus};
use sylvander_agent::spec::{AgentSpec, BehaviorConfig, PersonaConfig, ToolRef};
use sylvander_agent::tool::ToolRegistry;
use sylvander_agent::tools::{EditTool, MemoryReadTool, ReadTool, WriteTool};
use sylvander_agent::tools::memory::InMemoryMemoryStore;
use sylvander_channel::Channel;
use sylvander_llm_anthropic::api::client::AnthropicClient;
use sylvander_llm_anthropic::api::model::{ModelCapabilities, ModelInfo};
use tracing::info;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.into())
}

fn require_env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} must be set"))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    info!("sylvander server starting");

    // ── Config ──
    let model_name = env_or("SYLVANDER_MODEL", "claude-sonnet-5-20260601");
    let client = AnthropicClient::builder()
        .api_key(require_env("ANTHROPIC_API_KEY"))
        .base_url(env_or("ANTHROPIC_BASE_URL", "https://api.anthropic.com"))
        .build()
        .expect("client");

    let spec = AgentSpec::builder()
        .id("assistant")
        .name("Assistant")
        .persona(PersonaConfig {
            system_prompt: "You are a helpful assistant. You can read/write/edit files and search your memory with read_memory.".into(),
            description: "Default assistant".into(),
        })
        .tools(vec![
            ToolRef::Builtin { name: "read".into() },
            ToolRef::Builtin { name: "write".into() },
            ToolRef::Builtin { name: "edit".into() },
        ])
        .behavior(BehaviorConfig { max_iterations: 30, max_retries: 3 })
        .build()
        .expect("spec");

    let _model = ModelInfo::builder()
        .id(&model_name)
        .context_window(200_000)
        .max_output_tokens(32_000)
        .capability(ModelCapabilities::TOOL_USE)
        .build()
        .expect("model");

    let memory = Arc::new(InMemoryMemoryStore::new());
    let tools = ToolRegistry::new()
        .register(ReadTool::new("/"))
        .register(WriteTool::new("/"))
        .register(EditTool::new("/"))
        .register(MemoryReadTool::new(memory));

    // ── Bus ──
    let bus = Arc::new(InProcessMessageBus::new());

    // ── Agent ──
    let mut run_builder = sylvander_agent::run::AgentRun::builder(spec.clone(), client.clone())
        .bus(bus.clone())
        .override_tools(tools)
        .model_capabilities(ModelCapabilities::TOOL_USE);

    if std::env::var("SYLVANDER_APPROVAL").is_ok() {
        run_builder = run_builder.enable_approval();
    }

    let run = run_builder.build().expect("agent build");
    let agent_id = run.id().clone();

    let filter = run.subscription_filter();
    let inbox = bus.subscribe(filter).await.expect("subscribe");
    let agent_task = tokio::spawn(async move { run.run(inbox).await });

    info!(%agent_id, "agent spawned");

    // ── DingTalk channel ──
    let dt_key = std::env::var("DINGTALK_APP_KEY");
    let dt_secret = std::env::var("DINGTALK_APP_SECRET");

    if let (Ok(app_key), Ok(app_secret)) = (dt_key, dt_secret) {
        let channel = Arc::new(
            sylvander_channel_dingtalk::DingTalkChannel::new(&app_key, &app_secret),
        );
        let ctx = sylvander_channel::ChannelContext {
            bus: bus.clone(),
            sessions: Arc::new(sylvander_agent::session_store::InMemorySessionStore::new()),
        };
        let ch = channel.clone();
        tokio::spawn(async move { ch.run(ctx).await });
        info!("dingtalk channel started");
    } else {
        info!("dingtalk not configured (set DINGTALK_APP_KEY + DINGTALK_APP_SECRET)");
    }

    info!("sylvander server running — Ctrl+C to stop");

    // ── Wait ──
    tokio::signal::ctrl_c().await.expect("ctrl_c");
    info!("shutting down...");
    agent_task.abort();
    info!("stopped");
}
