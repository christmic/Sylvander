use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::Response;
use bytes::Bytes;
use futures::StreamExt;
use tokio::sync::mpsc;

use crate::AppState;
use crate::error::AppError;
use crate::metering::{self, MeterMeta};
use crate::select::{self, Attempt};
use crate::store::RequestRow;

/// Headers we never forward as-is (transport / auth are handled explicitly).
fn is_hop_or_auth(name: &str) -> bool {
    matches!(
        name,
        "host" | "content-length" | "connection" | "x-api-key" | "authorization"
    )
}

/// Router entrypoint: resolve the logical model to an ordered attempt list,
/// then forward with fallback (rules-first, rate-limit-aware, load-balanced).
/// Records the full routing decision (attempts + reason + trail) for analysis.
pub async fn proxy(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let start = Instant::now();
    let ts = now_ms();
    let id = uuid::Uuid::now_v7().to_string();

    let tool = {
        let rules = state.tools.read().await;
        crate::tool::logical(&headers, &rules)
    };
    let tool_raw = crate::tool::raw(&headers);

    let json: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| AppError::bad_request("request body is not valid JSON"))?;
    let model_id = json
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| AppError::bad_request("request body has no `model` field"))?
        .to_string();

    let routeset = {
        let rt = state.routes.read().await;
        rt.resolve(&model_id)
    }
    .ok_or_else(|| {
        AppError::new(
            StatusCode::NOT_FOUND,
            format!("no route for model `{model_id}`"),
        )
    })?;

    let stream = json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    // Plan the ordered attempts (rate-limit snapshot + load-balance state).
    let rl = state.store.list_rate_limits().await.unwrap_or_default();
    let attempts = select::plan(&routeset, &model_id, &rl, &state.lb);
    if attempts.is_empty() {
        return Err(AppError::new(
            StatusCode::NOT_FOUND,
            format!("no usable target for `{model_id}`"),
        ));
    }

    let path = uri.path();
    let query = uri.query();

    let mut trail: Vec<serde_json::Value> = Vec::new();
    let mut committed: Option<(reqwest::Response, Attempt)> = None;
    let mut tried = 0usize;
    let mut last_err = String::new();

    for attempt in attempts {
        tried += 1;
        let url = match query {
            Some(q) => format!("{}{}?{}", attempt.base_url, path, q),
            None => format!("{}{}", attempt.base_url, path),
        };
        let out_body = build_body(&json, &body, &attempt, stream)?;

        let mut req = state.http.request(method.clone(), url);
        for (k, v) in headers.iter() {
            if is_hop_or_auth(k.as_str()) {
                continue;
            }
            req = req.header(k.clone(), v.clone());
        }
        if let Some(token) = &attempt.token {
            if attempt.dialect.is_openai() {
                req = req.header("authorization", format!("Bearer {token}"));
            } else {
                req = req.header("x-api-key", token.clone());
            }
        }
        req = req.body(out_body);

        match req.send().await {
            Err(e) => {
                last_err = e.to_string();
                trail.push(trail_entry(&attempt, &format!("error:{e}")));
            }
            Ok(resp) => {
                let st = resp.status();
                // Fallback-eligible before we start streaming: 429 / 5xx / transport.
                if st.as_u16() == 429 || st.is_server_error() {
                    last_err = format!("upstream {}", st.as_u16());
                    trail.push(trail_entry(&attempt, &format!("http:{}", st.as_u16())));
                } else {
                    trail.push(trail_entry(&attempt, "ok"));
                    committed = Some((resp, attempt));
                    break;
                }
            }
        }
    }

    let (resp, attempt) = match committed {
        Some(x) => x,
        None => {
            record_failure(
                &state, &id, ts, start, &model_id, stream, &tool, &tool_raw, tried, &trail,
                &last_err,
            );
            return Err(AppError::bad_gateway(format!(
                "all targets failed: {last_err}"
            )));
        }
    };

    let trail_json = serde_json::to_string(&trail).ok();
    let status = resp.status();
    let resp_headers = resp.headers().clone();
    let upstream = resp.bytes_stream();

    // Off-path: capture vendor rate-limit headers from the committed response.
    if let Some(snap) = crate::ratelimit::parse(attempt.dialect, &resp_headers) {
        let store = state.store.clone();
        let provider = attempt.provider.clone();
        tokio::spawn(async move {
            let _ = store.upsert_rate_limit(&provider, &snap).await;
        });
    }

    // Off-path metering, recording the actually-used target + routing decision.
    let (tx, rx) = mpsc::unbounded_channel::<Bytes>();
    let meta = MeterMeta {
        id,
        ts,
        start,
        dialect: attempt.dialect,
        model_id,
        provider: attempt.provider.clone(),
        real_model: attempt.real_model.clone(),
        stream,
        status: status.as_u16() as i64,
        tool: tool.clone(),
        tool_raw: Some(tool_raw.clone()),
        attempts: tried as i64,
        route_reason: Some(attempt.reason.to_string()),
        route_trail: trail_json,
    };
    tokio::spawn(metering::run(rx, meta, state.store.clone()));

    let teed = upstream.map(move |item| {
        if let Ok(ref chunk) = item {
            let _ = tx.send(chunk.clone());
        }
        item
    });

    let mut response = Response::builder()
        .status(status)
        .body(Body::from_stream(teed))
        .map_err(|e| AppError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let out_headers = response.headers_mut();
    for (k, v) in resp_headers.iter() {
        let name = k.as_str();
        if name == "content-length" || name == "transfer-encoding" {
            continue;
        }
        out_headers.insert(k.clone(), v.clone());
    }
    Ok(response)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn trail_entry(a: &Attempt, outcome: &str) -> serde_json::Value {
    serde_json::json!({
        "provider": a.provider,
        "real_model": a.real_model,
        "reason": a.reason,
        "outcome": outcome,
    })
}

/// Build the outbound body for an attempt. Verbatim unless model rewrite or
/// (opt-in) OpenAI-chat usage injection is required.
fn build_body(
    json: &serde_json::Value,
    body: &Bytes,
    attempt: &Attempt,
    stream: bool,
) -> Result<Bytes, AppError> {
    let inject = attempt.inject_usage
        && matches!(attempt.dialect, crate::config::Dialect::OpenaiChat)
        && stream;
    if !attempt.rewrite_model && !inject {
        return Ok(body.clone());
    }
    let mut j = json.clone();
    if attempt.rewrite_model {
        j["model"] = serde_json::Value::String(attempt.real_model.clone());
    }
    if inject
        && let Some(so) = j.as_object_mut().and_then(|o| {
            o.entry("stream_options")
                .or_insert(serde_json::json!({}))
                .as_object_mut()
        })
    {
        so.insert("include_usage".into(), serde_json::Value::Bool(true));
    }
    let vec = serde_json::to_vec(&j).map_err(|e| {
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("serialize error: {e}"),
        )
    })?;
    Ok(Bytes::from(vec))
}

#[allow(clippy::too_many_arguments)]
fn record_failure(
    state: &AppState,
    id: &str,
    ts: i64,
    start: Instant,
    model_id: &str,
    stream: bool,
    tool: &str,
    tool_raw: &str,
    tried: usize,
    trail: &[serde_json::Value],
    err: &str,
) {
    let row = RequestRow {
        id: id.to_string(),
        ts,
        client_protocol: "unknown".to_string(),
        model_id: model_id.to_string(),
        provider: "-".to_string(),
        real_model: model_id.to_string(),
        stream,
        status: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_write_tokens: 0,
        cache_read_tokens: 0,
        latency_ms: Some(start.elapsed().as_millis() as i64),
        ttft_ms: None,
        error: Some(err.to_string()),
        tool: tool.to_string(),
        tool_raw: Some(tool_raw.to_string()),
        attempts: tried as i64,
        route_reason: Some("failed".to_string()),
        route_trail: serde_json::to_string(trail).ok(),
    };
    let store = state.store.clone();
    tokio::spawn(async move {
        let _ = store.record(row).await;
    });
}
