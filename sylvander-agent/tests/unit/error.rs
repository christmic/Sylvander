use super::*;
use sylvander_llm_core::{ProviderErrorKind, ProviderErrorPhase};

#[test]
fn max_iterations_display() {
    let err = AgentLoopError::MaxIterationsReached(50);
    assert!(format!("{err}").contains("50"));
    assert!(!err.is_retryable());
    assert_eq!(err.status(), None);
}

#[test]
fn incompatible_model_display() {
    let err = AgentLoopError::IncompatibleModel("model lacks TOOL_USE".into());
    assert!(!err.is_retryable());
}

#[test]
fn provider_4xx_not_retryable() {
    let mut source = ProviderError::new(
        ProviderErrorKind::InvalidRequest,
        ProviderErrorPhase::Open,
        "bad input",
    );
    source.status = Some(400);
    let err = AgentLoopError::Provider {
        attempts: 1,
        source,
    };
    assert!(!err.is_retryable());
    assert_eq!(err.status(), Some(400));
}

#[test]
fn provider_429_is_retryable() {
    let mut source = ProviderError::new(
        ProviderErrorKind::RateLimited,
        ProviderErrorPhase::Open,
        "slow down",
    );
    source.status = Some(429);
    let err = AgentLoopError::Provider {
        attempts: 3,
        source,
    };
    assert!(err.is_retryable());
    assert_eq!(err.status(), Some(429));
    assert!(format!("{err}").contains("3 attempts"));
}

#[test]
fn provider_5xx_is_retryable() {
    let mut source = ProviderError::new(
        ProviderErrorKind::Unavailable,
        ProviderErrorPhase::Open,
        "overloaded",
    );
    source.status = Some(503);
    let err = AgentLoopError::Provider {
        attempts: 4,
        source,
    };
    assert!(err.is_retryable());
    assert_eq!(err.status(), Some(503));
}

#[test]
fn provider_retryability_and_status_are_typed() {
    let mut source = ProviderError::new(
        ProviderErrorKind::RateLimited,
        ProviderErrorPhase::Open,
        "model provider rate limit reached",
    );
    source.status = Some(429);
    let err = AgentLoopError::Provider {
        attempts: 2,
        source,
    };
    assert!(err.is_retryable());
    assert_eq!(err.status(), Some(429));
    assert!(format!("{err}").contains("2 attempts"));

    let err = AgentLoopError::Provider {
        attempts: 1,
        source: ProviderError::new(
            ProviderErrorKind::Authentication,
            ProviderErrorPhase::Open,
            "model provider authentication failed",
        ),
    };
    assert!(!err.is_retryable());
    assert_eq!(err.status(), None);
}

#[test]
fn tool_error_not_retryable() {
    let err = AgentLoopError::Tool("panic in user tool".into());
    assert!(!err.is_retryable());
}

#[test]
fn compression_error_not_retryable() {
    let err = AgentLoopError::Compression("invalid threshold".into());
    assert!(!err.is_retryable());
}

#[test]
fn validation_error_not_retryable() {
    let err = AgentLoopError::Validation("messages empty".into());
    assert!(!err.is_retryable());
}

#[test]
fn builder_error_not_retryable() {
    let err = AgentLoopError::Builder("missing client".into());
    assert!(!err.is_retryable());
}
