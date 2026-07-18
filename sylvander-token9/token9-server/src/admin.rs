use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use token9_contracts::{
    ModelDto, ModelsResponse, ProviderDto, ProvidersResponse, ToolRuleDto, ToolRulesResponse,
};

use crate::AppState;
use crate::config::Dialect;
use crate::error::AppError;
use crate::routetable::RouteTable;

/// Rebuild the in-memory route + tool-rule caches from the DB. Called after
/// every mutation so a running server reflects changes immediately.
async fn reload(state: &AppState) -> Result<usize, AppError> {
    let rt = RouteTable::load(&state.store)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let n = rt.len();
    *state.routes.write().await = rt;

    let rules = state
        .store
        .load_tool_rules()
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    *state.tools.write().await = rules;

    Ok(n)
}

/// Mask a secret for display: keep last 4 chars. `None` stays `None`.
fn mask(key: &Option<String>) -> Option<String> {
    match key {
        Some(k) if k.len() > 4 => Some(format!("****{}", &k[k.len() - 4..])),
        Some(_) => Some("****".into()),
        None => None,
    }
}

// ---- providers ----

#[derive(Debug, Deserialize)]
pub struct ProviderInput {
    pub name: String,
    pub base_url: String,
    pub dialect: String,
    #[serde(default)]
    pub api_key: Option<String>,
}

pub async fn list_providers(
    State(state): State<AppState>,
) -> Result<Json<ProvidersResponse>, AppError> {
    let ps = state
        .store
        .list_providers()
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let providers = ps
        .into_iter()
        .map(|p| ProviderDto {
            api_key: mask(&p.api_key),
            name: p.name,
            base_url: p.base_url,
            dialect: p.dialect,
        })
        .collect();
    Ok(Json(ProvidersResponse { providers }))
}

pub async fn create_provider(
    State(state): State<AppState>,
    Json(input): Json<ProviderInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let dialect = Dialect::from_str(&input.dialect).map_err(AppError::bad_request)?;
    state
        .store
        .add_provider(
            &input.name,
            &input.base_url,
            dialect,
            input.api_key.as_deref(),
        )
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let routes = reload(&state).await?;
    Ok(Json(serde_json::json!({ "ok": true, "routes": routes })))
}

pub async fn delete_provider(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let removed = state
        .store
        .remove_provider(&name)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let routes = reload(&state).await?;
    Ok(Json(
        serde_json::json!({ "removed": removed, "routes": routes }),
    ))
}

// ---- models ----

#[derive(Debug, Deserialize)]
pub struct ModelInput {
    pub model_id: String,
    pub provider: String,
    pub real_model: String,
    #[serde(default)]
    pub inject_usage: bool,
}

pub async fn list_models(State(state): State<AppState>) -> Result<Json<ModelsResponse>, AppError> {
    let ms = state
        .store
        .list_models()
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let models = ms
        .into_iter()
        .map(|m| ModelDto {
            model_id: m.model_id,
            provider: m.provider,
            real_model: m.real_model,
            inject_usage: m.inject_usage,
        })
        .collect();
    Ok(Json(ModelsResponse { models }))
}

pub async fn create_model(
    State(state): State<AppState>,
    Json(input): Json<ModelInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    state
        .store
        .add_model(
            &input.model_id,
            &input.provider,
            &input.real_model,
            input.inject_usage,
        )
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    let routes = reload(&state).await?;
    Ok(Json(serde_json::json!({ "ok": true, "routes": routes })))
}

pub async fn delete_model(
    State(state): State<AppState>,
    Path(model_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let removed = state
        .store
        .remove_model(&model_id)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let routes = reload(&state).await?;
    Ok(Json(
        serde_json::json!({ "removed": removed, "routes": routes }),
    ))
}

// ---- tool rules ----

#[derive(Debug, Deserialize)]
pub struct ToolRuleInput {
    pub label: String,
    #[serde(default = "default_header")]
    pub header: String,
    pub pattern: String,
    #[serde(default = "default_priority")]
    pub priority: i64,
}

fn default_header() -> String {
    "user-agent".to_string()
}
fn default_priority() -> i64 {
    100
}

pub async fn list_tools(
    State(state): State<AppState>,
) -> Result<Json<ToolRulesResponse>, AppError> {
    let rs = state
        .store
        .list_tool_rules()
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rules = rs
        .into_iter()
        .map(|r| ToolRuleDto {
            id: r.id,
            label: r.label,
            header: r.header,
            pattern: r.pattern,
            priority: r.priority,
        })
        .collect();
    Ok(Json(ToolRulesResponse { rules }))
}

pub async fn create_tool(
    State(state): State<AppState>,
    Json(input): Json<ToolRuleInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let id = state
        .store
        .add_tool_rule(&input.label, &input.header, &input.pattern, input.priority)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    reload(&state).await?;
    Ok(Json(serde_json::json!({ "ok": true, "id": id })))
}

pub async fn delete_tool(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let removed = state
        .store
        .remove_tool_rule(id)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    reload(&state).await?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

// ---- provider keys ----

#[derive(Debug, Deserialize)]
pub struct KeyInput {
    pub provider: String,
    pub api_key: String,
    #[serde(default)]
    pub label: Option<String>,
}

pub async fn list_keys(State(state): State<AppState>) -> Result<Json<serde_json::Value>, AppError> {
    let ks = state
        .store
        .list_provider_keys(None)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rows: Vec<_> = ks
        .into_iter()
        .map(|k| {
            serde_json::json!({
                "id": k.id, "provider": k.provider, "label": k.label,
                "enabled": k.enabled, "api_key": mask(&k.api_key),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "keys": rows })))
}

pub async fn create_key(
    State(state): State<AppState>,
    Json(input): Json<KeyInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    state
        .store
        .add_provider_key(&input.provider, &input.api_key, input.label.as_deref())
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    reload(&state).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_key(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let removed = state
        .store
        .remove_provider_key(id)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    reload(&state).await?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

// ---- routes (targets) ----

#[derive(Debug, Deserialize)]
pub struct RouteInput {
    pub model_id: String,
    pub provider: String,
    pub real_model: String,
    #[serde(default = "one")]
    pub weight: i64,
    #[serde(default = "hundred")]
    pub priority: i64,
}

fn one() -> i64 {
    1
}
fn hundred() -> i64 {
    100
}

pub async fn list_routes(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let rs = state
        .store
        .list_routes()
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rows: Vec<_> = rs
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id, "model_id": r.model_id, "provider": r.provider,
                "real_model": r.real_model, "weight": r.weight,
                "priority": r.priority, "enabled": r.enabled,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "routes": rows })))
}

pub async fn create_route(
    State(state): State<AppState>,
    Json(input): Json<RouteInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    state
        .store
        .add_route(
            &input.model_id,
            &input.provider,
            &input.real_model,
            input.weight,
            input.priority,
        )
        .await
        .map_err(|e| AppError::bad_request(e.to_string()))?;
    reload(&state).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_route(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AppError> {
    let removed = state
        .store
        .remove_route(id)
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    reload(&state).await?;
    Ok(Json(serde_json::json!({ "removed": removed })))
}

// ---- reload ----

pub async fn reload_routes(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let routes = reload(&state).await?;
    Ok(Json(serde_json::json!({ "ok": true, "routes": routes })))
}
