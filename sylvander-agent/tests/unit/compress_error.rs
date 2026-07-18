use super::*;

#[test]
fn compatibility_reasons_are_bounded_and_do_not_echo_sources() {
    for code in [
        CompactionFailureCode::Busy,
        CompactionFailureCode::InsufficientHistory,
        CompactionFailureCode::Provider,
        CompactionFailureCode::Protocol,
        CompactionFailureCode::Persistence,
        CompactionFailureCode::UnsupportedBackend,
        CompactionFailureCode::SessionUnavailable,
        CompactionFailureCode::Other,
    ] {
        let reason = CompactionError::new(code).compatibility_reason();
        assert!(!reason.is_empty() && reason.len() <= 96);
        assert!(!reason.contains("secret"));
    }
    let source = sylvander_llm_core::ProviderError::new(
        sylvander_llm_core::ProviderErrorKind::Authentication,
        sylvander_llm_core::ProviderErrorPhase::Open,
        "secret-token-value",
    );
    let error = CompactionError::from_loop(&AgentLoopError::Provider {
        attempts: 1,
        source,
    });
    assert_eq!(error.code, CompactionFailureCode::Provider);
    assert!(!error.compatibility_reason().contains("secret-token-value"));

    let protocol = AgentLoopError::Provider {
        attempts: 1,
        source: sylvander_llm_core::ProviderError::new(
            sylvander_llm_core::ProviderErrorKind::Protocol,
            sylvander_llm_core::ProviderErrorPhase::Stream,
            "secret protocol payload",
        ),
    };
    assert_eq!(
        CompactionError::from_loop(&protocol).code,
        CompactionFailureCode::Protocol
    );
}
