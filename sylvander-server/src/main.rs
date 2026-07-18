//! Sylvander server composition root.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use sylvander_agent::bus::{
    ApprovalPolicy, FileAccess, ModelCapability, ModelDescriptor, ModelLifecycle, NetworkAccess,
    PermissionProfile, ReasoningEffort, SessionConfigOverrides, SessionWorkspaceBinding,
};
use sylvander_agent::spec::AgentId;
use sylvander_channel::Channel;
use sylvander_channel::credential::CredentialLeaseSource;
use sylvander_runtime::config::{
    ChannelTransportConfig, SecretRef, ServerConfig, SystemSecretResolver,
};
use sylvander_runtime::{ChannelRegistration, ChannelRestartPolicy, ChannelStatus, Runtime};
use tracing::info;

mod credential;

use credential::SystemChannelCredentialSource;

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    init_tracing();

    let config = load_config()?;
    let runtime = Arc::new(Runtime::boot_config(config.clone()).await?);
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

fn init_tracing() {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    if std::env::var("SYLVANDER_LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_env_filter(filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
}

fn load_config() -> Result<ServerConfig, ServerError> {
    let path = required_config_path(std::env::var_os("SYLVANDER_CONFIG"))?;
    let config = ServerConfig::load(&path)?;
    info!(path = %path.display(), "server configuration loaded");
    Ok(config)
}

fn required_config_path(value: Option<OsString>) -> Result<PathBuf, ServerError> {
    value
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or(ServerError::MissingConfig)
}

fn build_channels(
    config: &ServerConfig,
    runtime: &Arc<Runtime>,
) -> Result<Vec<ChannelRegistration>, ServerError> {
    let credential_audit = runtime.credential_audit_ledger().ok_or_else(|| {
        ServerError::Runtime(sylvander_runtime::RuntimeError::Store(
            "credential audit ledger is unavailable".into(),
        ))
    })?;
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
                                model: primary.0.clone(),
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
                } => {
                    let health_runtime = Arc::clone(runtime);
                    Arc::new(
                        sylvander_channel_http::HttpChannel::new(parse_addr(bind)?, agent_id)
                        .with_request_limit(config.server.boundary.max_request_bytes)
                        .with_bearer_lease(
                            &channel.id,
                            principal_id,
                            channel_credential_source(
                                &channel.id,
                                [("bearer_token", bearer_token)],
                                credential_audit.clone(),
                            )?,
                        )
                        .map_err(|error| ServerError::Channel {
                            id: channel.id.clone(),
                            message: error.to_string(),
                        })?
                        .with_operational_health(Arc::new(move || {
                            let runtime = Arc::clone(&health_runtime);
                            Box::pin(async move {
                                let snapshot = runtime
                                    .operational_snapshot()
                                    .await
                                    .map_err(|error| error.to_string())?;
                                Ok(sylvander_channel_http::OperationalHealth {
                                    ready: snapshot.ready,
                                    agents: snapshot.agent_count,
                                    persistent_sessions: snapshot.persistent_session_count,
                                    ready_channels: snapshot
                                        .channels
                                        .iter()
                                        .filter(|channel| channel.status == ChannelStatus::Ready)
                                        .count(),
                                    total_channels: snapshot.channels.len(),
                                    bus_subscribers: snapshot.bus.subscriber_count,
                                    bus_capacity: snapshot.bus.subscription_capacity,
                                    published_messages: snapshot.bus.published_messages,
                                    backpressure_rejections: snapshot
                                        .bus
                                        .backpressure_rejections,
                                })
                            })
                        })),
                    )
                }
                ChannelTransportConfig::Websocket {
                    bind,
                    principal_id,
                    bearer_token,
                } => Arc::new(
                    sylvander_channel_ws::WsChannel::new(parse_addr(bind)?, agent_id)
                        .with_request_limit(config.server.boundary.max_request_bytes)
                        .with_bearer_lease(
                            &channel.id,
                            principal_id,
                            channel_credential_source(
                                &channel.id,
                                [("bearer_token", bearer_token)],
                                credential_audit.clone(),
                            )?,
                        )
                        .map_err(|error| ServerError::Channel {
                            id: channel.id.clone(),
                            message: error.to_string(),
                        })?,
                ),
                ChannelTransportConfig::DingTalk {
                    app_key,
                    app_secret,
                } => Arc::new(
                    sylvander_channel_dingtalk::DingTalkChannel::new(
                        &channel.id,
                        agent_id,
                        channel_credential_source(
                            &channel.id,
                            [("app_key", app_key), ("app_secret", app_secret)],
                            credential_audit.clone(),
                        )?,
                    )
                    .map_err(|error| ServerError::Channel {
                        id: channel.id.clone(),
                        message: error.to_string(),
                    })?
                    .with_request_limit(config.server.boundary.max_request_bytes),
                ),
                ChannelTransportConfig::Telegram {
                    token,
                    bind,
                    webhook_secret,
                } => Arc::new(
                    sylvander_channel_telegram::TelegramChannel::new(
                        parse_addr(bind)?,
                        agent_id,
                        &channel.id,
                        channel_credential_source(
                            &channel.id,
                            [("bot_token", token), ("webhook_secret", webhook_secret)],
                            credential_audit.clone(),
                        )?,
                    )
                    .map_err(|error| ServerError::Channel {
                        id: channel.id.clone(),
                        message: error.to_string(),
                    })?
                    .with_request_limit(config.server.boundary.max_request_bytes),
                ),
                ChannelTransportConfig::Wechat {
                    bind,
                    corp_id,
                    agent_id: wechat_agent_id,
                    secret,
                    token,
                    encoding_aes_key,
                } => Arc::new(
                    sylvander_channel_wechat::WechatChannel::new(
                        corp_id.clone(),
                        wechat_agent_id.clone(),
                        parse_addr(bind)?,
                        agent_id,
                        &channel.id,
                        channel_credential_source(
                            &channel.id,
                            [
                                ("api_secret", secret),
                                ("callback_token", token),
                                ("encoding_aes_key", encoding_aes_key),
                            ],
                            credential_audit.clone(),
                        )?,
                    )
                    .map_err(|message| ServerError::Channel {
                        id: channel.id.clone(),
                        message,
                    })?
                    .with_request_limit(config.server.boundary.max_request_bytes),
                ),
            };
            info!(instance = %channel.id, kind = result.name(), "channel configured");
            let defaults = SessionConfigOverrides {
                user_workspace: channel.default_workspace.as_ref().map(|workspace| {
                    SessionWorkspaceBinding {
                        execution_target: workspace.execution_target.clone(),
                        path: workspace.path.clone().into(),
                        read_only: workspace.read_only,
                        instruction_focus: workspace
                            .instruction_focus
                            .as_ref()
                            .map(std::path::PathBuf::from),
                    }
                }),
                ..SessionConfigOverrides::default()
            };
            Ok(
                ChannelRegistration::new(&channel.id, result)
                    .with_session_defaults(defaults)
                    .with_restart_policy(
                    ChannelRestartPolicy {
                        max_attempts: channel.supervision.max_restart_attempts,
                        initial_backoff: std::time::Duration::from_millis(
                            channel.supervision.initial_backoff_ms,
                        ),
                        max_backoff: std::time::Duration::from_millis(
                            channel.supervision.max_backoff_ms,
                        ),
                    }),
            )
        })
        .collect()
}

fn channel_credential_source<'a>(
    channel_id: &str,
    references: impl IntoIterator<Item = (&'a str, &'a SecretRef)>,
    audit: Arc<sylvander_runtime::credential_audit::CredentialOperationAuditLedger>,
) -> Result<Arc<dyn CredentialLeaseSource>, ServerError> {
    SystemChannelCredentialSource::new(
        channel_id,
        references
            .into_iter()
            .map(|(slot, reference)| (slot.to_owned(), reference.clone())),
        Arc::new(SystemSecretResolver),
        audit,
    )
    .map(|source| Arc::new(source) as Arc<dyn CredentialLeaseSource>)
    .map_err(|error| ServerError::Channel {
        id: channel_id.to_owned(),
        message: error.to_string(),
    })
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
    #[error("SYLVANDER_CONFIG must name the latest-version server configuration")]
    MissingConfig,
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

#[cfg(test)]
#[path = "../tests/unit/server_main.rs"]
mod tests;
