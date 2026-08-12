//! Provider-neutral conversion boundary for the two `OpenAI` protocols.

mod chat;
mod common;
mod responses;

use sylvander_llm_core::{ProviderError, ProviderErrorKind, ProviderErrorPhase};

use crate::api::OpenAiError;

pub(crate) use chat::{chat_request, chat_response};
pub(crate) use responses::{responses_request, responses_response};

pub(crate) fn error(error: OpenAiError, phase: ProviderErrorPhase) -> ProviderError {
    let kind = match &error {
        OpenAiError::Http(source) if source.is_timeout() => ProviderErrorKind::Timeout,
        OpenAiError::Http(_) => ProviderErrorKind::Transport,
        OpenAiError::Api { status: 401, .. } => ProviderErrorKind::Authentication,
        OpenAiError::Api { status: 403, .. } => ProviderErrorKind::PermissionDenied,
        OpenAiError::Api { status: 404, .. } => ProviderErrorKind::ModelNotFound,
        OpenAiError::Api { status: 429, .. } => ProviderErrorKind::RateLimited,
        OpenAiError::Api { status, .. } if *status >= 500 => ProviderErrorKind::Unavailable,
        OpenAiError::Api { .. } => ProviderErrorKind::InvalidRequest,
        OpenAiError::Json(_) | OpenAiError::Sse(_) | OpenAiError::Protocol(_) => {
            ProviderErrorKind::Protocol
        }
    };
    let mut output = ProviderError::new(kind, phase, "OpenAI provider request failed");
    output.status = error.status();
    output.request_id = error.request_id().map(str::to_owned);
    output.retry_after_ms = error.retry_after_ms();
    output
}

pub(crate) fn invalid(message: &'static str) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        ProviderErrorPhase::Open,
        message,
    )
}

pub(crate) fn protocol(message: &'static str) -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Protocol,
        ProviderErrorPhase::Stream,
        message,
    )
}
