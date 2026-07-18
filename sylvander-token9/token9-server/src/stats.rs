use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use token9_contracts::{ObservedToolDto, ObservedToolsResponse, StatBucketDto, StatsResponse};

use crate::AppState;
use crate::error::AppError;

#[derive(Debug, Deserialize)]
pub struct StatsQuery {
    /// Inclusive lower bound, YYYY-MM-DD.
    pub from: Option<String>,
    /// Inclusive upper bound, YYYY-MM-DD.
    pub to: Option<String>,
}

/// GET /stats/summary — usage grouped by provider + model + date.
/// Dimensions: provider, model, requests, input/output tokens, cache_ratio, date.
pub async fn summary(
    State(state): State<AppState>,
    Query(q): Query<StatsQuery>,
) -> Result<Json<StatsResponse>, AppError> {
    let buckets = state
        .store
        .stats(q.from.as_deref(), q.to.as_deref())
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let buckets = buckets
        .into_iter()
        .map(|b| {
            let denom = b.input_tokens + b.cache_read_tokens;
            let cache_ratio = if denom > 0 {
                (b.cache_read_tokens as f64 / denom as f64 * 10000.0).round() / 10000.0
            } else {
                0.0
            };
            StatBucketDto {
                provider: b.provider,
                model: b.real_model,
                tool: b.tool,
                date: b.date,
                requests: b.requests,
                input_tokens: b.input_tokens,
                output_tokens: b.output_tokens,
                cache_read_tokens: b.cache_read_tokens,
                cache_write_tokens: b.cache_write_tokens,
                cache_ratio,
            }
        })
        .collect();

    Ok(Json(StatsResponse { buckets }))
}

/// GET /tools/observed — distinct real tool identifiers seen in traffic with
/// their logical mapping. Rows where `tool == "OTHER"` are unmapped tools you
/// can add a rule for.
pub async fn observed_tools(
    State(state): State<AppState>,
) -> Result<Json<ObservedToolsResponse>, AppError> {
    let rows = state
        .store
        .observed_tools()
        .await
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let observed = rows
        .into_iter()
        .map(|o| ObservedToolDto {
            tool_raw: o.tool_raw,
            tool: o.tool,
            requests: o.requests,
        })
        .collect();
    Ok(Json(ObservedToolsResponse { observed }))
}
