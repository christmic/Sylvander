//! Sylvander server composition root.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use sylvander_agent::bus::{
    ApprovalPolicy, FileAccess, ModelCapability, ModelDescriptor, ModelLifecycle, NetworkAccess,
    PermissionProfile, ReasoningEffort,
};
use sylvander_agent::spec::AgentId;
use sylvander_channel::Channel;
use sylvander_runtime::Runtime;
use sylvander_runtime::config::{
    ChannelTransportConfig, SecretResolver, ServerConfig, SystemSecretResolver,
};
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config = load_config()?;
    let runtime = Runtime::boot_config(config.clone()).await?;
    let channels = build_channels(&config, &runtime)?;
    let channel_count = channels.len();
    runtime.start_channels(channels).await?;
    info!(
        server = %config.server.name,
        agents = config.agents.len(),
        channels = channel_count,
        "sylvander server running"
    );

    let runtime_failure = tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|error| ServerError::Signal(error.to_string()))?;
            info!("shutdown signal received");
            None
        }
        channel = runtime.wait_for_channel_exit() => channel.map(RuntimeFailure::Channel),
        agent = runtime.wait_for_agent_exit() => agent.map(RuntimeFailure::Agent),
    };
    let shutdown = runtime.shutdown().await;
    if let Some(failure) = runtime_failure {
        if let Err(error) = shutdown {
            tracing::warn!(%error, "runtime cleanup also reported an error");
        }
        return Err(match failure {
            RuntimeFailure::Channel(channel) => ServerError::ChannelStopped(channel),
            RuntimeFailure::Agent(agent) => ServerError::AgentStopped(agent.to_string()),
        });
    }
    shutdown?;
    Ok(())
}

fn load_config() -> Result<ServerConfig, ServerError> {
    if let Some(path) = std::env::var_os("SYLVANDER_CONFIG") {
        let path = Path::new(&path);
        let config = ServerConfig::load(path)?;
        info!(path = %path.display(), "server configuration loaded");
        Ok(config)
    } else {
        info!("SYLVANDER_CONFIG is unset; migrating legacy environment");
        ServerConfig::from_legacy_env().map_err(ServerError::from)
    }
}

fn build_channels(
    config: &ServerConfig,
    runtime: &Runtime,
) -> Result<Vec<Arc<dyn Channel>>, ServerError> {
    let secrets = SystemSecretResolver;
    config
        .channels
        .iter()
        .filter(|channel| channel.enabled)
        .map(|channel| {
            let agent_id = AgentId::new(&channel.default_agent);
            let agent = runtime
                .agent_descriptor(&agent_id)
                .ok_or_else(|| ServerError::UnknownAgent(channel.default_agent.clone()))?;
            let result: Arc<dyn Channel> = match &channel.transport {
                ChannelTransportConfig::Unix { path } => {
                    let primary = agent
                        .models
                        .iter()
                        .find(|(selection, _)| *selection == &agent.default_model)
                        .ok_or_else(|| {
                            ServerError::UnknownModel(agent.default_model.model_id.clone())
                        })?;
                    let models = agent
                        .models
                        .iter()
                        .map(|(selection, model)| ModelDescriptor {
                            id: selection.model_id.clone(),
                            provider: selection.provider_id.clone(),
                            capabilities: model.capabilities.bits(),
                            capability_names: model_capability_names(model.capabilities),
                            reasoning_efforts: if model
                                .capabilities
                                .contains(sylvander_llm_anthropic::api::model::ModelCapabilities::EXTENDED_THINKING)
                            {
                                vec![ReasoningEffort::Off, ReasoningEffort::Low, ReasoningEffort::Medium, ReasoningEffort::High]
                            } else {
                                vec![ReasoningEffort::Off]
                            },
                            lifecycle: ModelLifecycle::Active,
                            pricing: None,
                        })
                        .collect();
                    let platform_agent = agent.clone();
                    Arc::new(
                        sylvander_channel_unix::UnixChannel::new(path, agent_id)
                            .with_instance_id(&channel.id)
                            .with_request_limit(config.server.boundary.max_request_bytes)
                            .with_runtime_info(sylvander_channel_unix::RuntimeInfo {
                                model: primary.0.model_id.clone(),
                                reasoning_effort: ReasoningEffort::Off,
                                models,
                                permissions: PermissionProfile {
                                    file_access: FileAccess::WorkspaceWrite,
                                    network_access: NetworkAccess::Denied,
                                    approval_policy: if agent.approval_enabled {
                                        ApprovalPolicy::Ask
                                    } else {
                                        ApprovalPolicy::Allow
                                    },
                                },
                                capabilities: primary.1.capabilities.bits(),
                                approval_enabled: agent.approval_enabled,
                                max_attachment_bytes: 512 * 1024,
                                platform: agent.platform.clone(),
                                platform_provider: Some(Arc::new(move || {
                                    platform_agent.platform_snapshot()
                                })),
                            }),
                    )
                }
                ChannelTransportConfig::Http {
                    bind,
                    principal_id,
                    bearer_token,
                } => Arc::new(
                    sylvander_channel_http::HttpChannel::new(parse_addr(bind)?, agent_id)
                        .with_request_limit(config.server.boundary.max_request_bytes)
                        .with_bearer_auth(
                            &channel.id,
                            principal_id,
                            resolve_text(&secrets, bearer_token, &channel.id)?,
                        ),
                ),
                ChannelTransportConfig::Websocket {
                    bind,
                    principal_id,
                    bearer_token,
                } => Arc::new(
                    sylvander_channel_ws::WsChannel::new(parse_addr(bind)?, agent_id)
                        .with_request_limit(config.server.boundary.max_request_bytes)
                        .with_bearer_auth(
                            &channel.id,
                            principal_id,
                            resolve_text(&secrets, bearer_token, &channel.id)?,
                        ),
                ),
                ChannelTransportConfig::DingTalk {
                    app_key,
                    app_secret,
                } => Arc::new(sylvander_channel_dingtalk::DingTalkChannel::new(
                    resolve_text(&secrets, app_key, &channel.id)?,
                    resolve_text(&secrets, app_secret, &channel.id)?,
                )
                .with_identity(&channel.id, agent_id)
                .with_request_limit(config.server.boundary.max_request_bytes)),
                ChannelTransportConfig::Telegram {
                    token,
                    bind,
                    webhook_secret,
                } => Arc::new(
                    sylvander_channel_telegram::TelegramChannel::new(
                        resolve_text(&secrets, token, &channel.id)?,
                        parse_addr(bind)?,
                        agent_id,
                    )
                    .with_webhook_secret(resolve_text(
                        &secrets,
                        webhook_secret,
                        &channel.id,
                    )?)
                    .with_instance_id(&channel.id)
                    .with_request_limit(config.server.boundary.max_request_bytes),
                ),
                ChannelTransportConfig::Wechat {
                    bind,
                    corp_id,
                    secret,
                    token,
                    encoding_aes_key,
                    ..
                } => {
                    // Resolve the API credential now so startup fails before
                    // accepting traffic; outbound WeChat API support consumes it later.
                    let _api_secret = resolve_text(&secrets, secret, &channel.id)?;
                    Arc::new(
                        sylvander_channel_wechat::WechatChannel::new(
                            resolve_text(&secrets, token, &channel.id)?,
                            resolve_text(&secrets, encoding_aes_key, &channel.id)?,
                            corp_id.clone(),
                            parse_addr(bind)?,
                            agent_id,
                        )
                        .map_err(|message| ServerError::Channel {
                            id: channel.id.clone(),
                            message,
                        })?
                        .with_instance_id(&channel.id)
                        .with_request_limit(config.server.boundary.max_request_bytes),
                    )
                }
            };
            info!(instance = %channel.id, kind = result.name(), "channel configured");
            Ok(result)
        })
        .collect()
}

fn model_capability_names(
    capabilities: sylvander_llm_anthropic::api::model::ModelCapabilities,
) -> Vec<ModelCapability> {
    use sylvander_llm_anthropic::api::model::ModelCapabilities as Flags;
    [
        (Flags::EXTENDED_THINKING, ModelCapability::ExtendedThinking),
        (Flags::PROMPT_CACHING, ModelCapability::PromptCaching),
        (Flags::STRUCTURED_OUTPUT, ModelCapability::StructuredOutput),
        (Flags::TOOL_USE, ModelCapability::ToolUse),
        (Flags::VISION, ModelCapability::Vision),
        (Flags::DOCUMENT_INPUT, ModelCapability::DocumentInput),
    ]
    .into_iter()
    .filter_map(|(flag, name)| capabilities.contains(flag).then_some(name))
    .collect()
}

fn resolve_text(
    resolver: &dyn SecretResolver,
    reference: &sylvander_runtime::config::SecretRef,
    channel_id: &str,
) -> Result<String, ServerError> {
    let secret = resolver
        .resolve(reference)
        .map_err(|error| ServerError::Channel {
            id: channel_id.to_string(),
            message: error.to_string(),
        })?;
    secret
        .as_str()
        .map(str::to_string)
        .map_err(|error| ServerError::Channel {
            id: channel_id.to_string(),
            message: error.to_string(),
        })
}

fn parse_addr(value: &str) -> Result<SocketAddr, ServerError> {
    value
        .parse()
        .map_err(|error: std::net::AddrParseError| ServerError::Address {
            value: value.to_string(),
            message: error.to_string(),
        })
}

#[derive(Debug, thiserror::Error)]
enum ServerError {
    #[error(transparent)]
    Config(#[from] sylvander_runtime::config::ConfigError),
    #[error(transparent)]
    Runtime(#[from] sylvander_runtime::RuntimeError),
    #[error("configured channel references unavailable Agent `{0}`")]
    UnknownAgent(String),
    #[error("configured Agent references unavailable model `{0}`")]
    UnknownModel(String),
    #[error("channel `{id}` failed: {message}")]
    Channel { id: String, message: String },
    #[error("invalid socket address `{value}`: {message}")]
    Address { value: String, message: String },
    #[error("failed to wait for shutdown signal: {0}")]
    Signal(String),
    #[error("channel `{0}` exited while the server was running")]
    ChannelStopped(String),
    #[error("Agent `{0}` exited while the server was running")]
    AgentStopped(String),
}

enum RuntimeFailure {
    Channel(String),
    Agent(AgentId),
}
