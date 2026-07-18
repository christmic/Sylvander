use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Error surfaced to the client. Kept minimal — the gateway forwards upstream
/// errors verbatim; this type covers gateway-local failures only.
#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub msg: String,
}

impl AppError {
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self {
            status,
            msg: msg.into(),
        }
    }
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, msg)
    }
    pub fn bad_gateway(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, msg)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": { "message": self.msg } });
        (self.status, axum::Json(body)).into_response()
    }
}
