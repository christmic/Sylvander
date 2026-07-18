//! token9 proxy daemon and administration HTTP composition root.
//!
//! This binary wires persisted provider/model routing, metering, rate limits,
//! and the management API into one process. Business rules remain in the
//! owning modules so their contracts can be tested independently.

mod admin;
mod cli;
mod config;
mod error;
mod hosts;
mod metering;
mod proxy;
mod ratelimit;
mod routetable;
mod select;
mod stats;
mod store;
mod tool;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use clap::Parser;
use tokio::sync::RwLock;
use tracing::info;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Command};
use crate::config::Config;
use crate::routetable::RouteTable;
use crate::select::LbState;
use crate::store::sqlite::SqliteStore;
use crate::tool::ToolRule;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub store: Arc<SqliteStore>,
    pub routes: Arc<RwLock<RouteTable>>,
    pub tools: Arc<RwLock<Vec<ToolRule>>>,
    pub lb: Arc<LbState>,
    pub http: reqwest::Client,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let port_override = cli.port;
    let config = Config::load(&cli.config)?;

    match cli.command {
        None | Some(Command::Serve) => serve(config, port_override).await,
        Some(cmd) => {
            let store = SqliteStore::open(&config.db_path).await?;
            let settings_port = store
                .get_setting("port")
                .await?
                .and_then(|s| s.parse::<u16>().ok());
            let effective_port = port_override.or(settings_port).unwrap_or(config.port);

            let result = match cmd {
                Command::Provider { action } => cli::run_provider(&store, action).await,
                Command::Model { action } => cli::run_model(&store, action).await,
                Command::Tool { action } => cli::run_tool(&store, action).await,
                Command::Route { action } => cli::run_route(&store, action).await,
                Command::Settings { action } => cli::run_settings(&store, action).await,
                Command::Endpoint => cli::run_endpoint(&store, &config).await,
                Command::Hosts { action } => {
                    let domain = store
                        .get_setting("domain")
                        .await?
                        .unwrap_or(config.domain.clone());
                    cli::run_hosts(&domain, action)
                }
                Command::Serve => unreachable!(),
            };
            result?;

            cli::try_reload_server(effective_port).await;
            Ok(())
        }
    }
}

async fn serve(config: Config, port_override: Option<u16>) -> anyhow::Result<()> {
    let store = Arc::new(SqliteStore::open(&config.db_path).await?);
    store.seed_default_tool_rules().await?;
    // Preset domain/port into the settings table on first run.
    store.seed_setting("domain", &config.domain).await?;
    store.seed_setting("port", &config.port.to_string()).await?;
    let routes = RouteTable::load(&store).await?;
    let tools = store.load_tool_rules().await?;
    info!(
        routes = routes.len(),
        tool_rules = tools.len(),
        "loaded config"
    );
    let http = reqwest::Client::builder().build()?;

    // Effective settings: --port override > DB setting > bootstrap default.
    let domain = store
        .get_setting("domain")
        .await?
        .unwrap_or(config.domain.clone());
    let settings_port = store
        .get_setting("port")
        .await?
        .and_then(|s| s.parse::<u16>().ok());
    let port = port_override.or(settings_port).unwrap_or(config.port);
    let bind = config.bind.clone();

    // Ensure the branded domain resolves to loopback (prompts for auth if needed).
    hosts::ensure(&domain, port);
    let state = AppState {
        config: Arc::new(config),
        store,
        routes: Arc::new(RwLock::new(routes)),
        tools: Arc::new(RwLock::new(tools)),
        lb: Arc::new(LbState::default()),
        http,
    };

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/stats/summary", get(stats::summary))
        .route("/ratelimits", get(ratelimit::list))
        .route("/tools/observed", get(stats::observed_tools))
        .route(
            "/admin/providers",
            get(admin::list_providers).post(admin::create_provider),
        )
        .route("/admin/providers/{name}", delete(admin::delete_provider))
        .route(
            "/admin/models",
            get(admin::list_models).post(admin::create_model),
        )
        .route("/admin/models/{model_id}", delete(admin::delete_model))
        .route(
            "/admin/tools",
            get(admin::list_tools).post(admin::create_tool),
        )
        .route("/admin/tools/{id}", delete(admin::delete_tool))
        .route(
            "/admin/routes",
            get(admin::list_routes).post(admin::create_route),
        )
        .route("/admin/routes/{id}", delete(admin::delete_route))
        .route("/admin/keys", get(admin::list_keys).post(admin::create_key))
        .route("/admin/keys/{id}", delete(admin::delete_key))
        .route("/admin/reload", post(admin::reload_routes))
        .fallback(proxy::proxy)
        // No body size limit — token9 is a local trusted proxy; the upstream
        // provider enforces its own request size limits.
        .layer(DefaultBodyLimit::disable())
        .with_state(state);

    let addr = format!("{bind}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(%addr, %domain, "token9 listening (try http://{domain}:{port})");
    axum::serve(listener, app).await?;
    Ok(())
}
