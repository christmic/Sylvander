use super::*;

#[test]
fn capabilities_bitops() {
    let combined = ModelCapabilities::EXTENDED_THINKING | ModelCapabilities::TOOL_USE;
    assert!(combined.contains(ModelCapabilities::EXTENDED_THINKING));
    assert!(combined.contains(ModelCapabilities::TOOL_USE));
    assert!(!combined.contains(ModelCapabilities::PROMPT_CACHING));
}

#[test]
fn capabilities_intersects() {
    let a = ModelCapabilities::EXTENDED_THINKING | ModelCapabilities::TOOL_USE;
    let b = ModelCapabilities::TOOL_USE | ModelCapabilities::PROMPT_CACHING;
    assert!(a.intersects(b));
}

#[test]
fn empty_capabilities() {
    let empty = ModelCapabilities::empty();
    assert!(!empty.contains(ModelCapabilities::EXTENDED_THINKING));
    assert!(!empty.intersects(ModelCapabilities::TOOL_USE));
}

#[test]
fn model_info_builder_minimal() {
    let info = ModelInfo::builder()
        .id("custom-model-v1")
        .context_window(100_000)
        .max_output_tokens(8_000)
        .capability(ModelCapabilities::TOOL_USE)
        .build()
        .expect("build should succeed");
    assert_eq!(info.id, "custom-model-v1");
    assert_eq!(info.context_window, 100_000);
    assert_eq!(info.max_output_tokens, 8_000);
    assert!(info.capabilities.contains(ModelCapabilities::TOOL_USE));
    assert!(info.cache_ttl.is_empty());
}

#[test]
fn model_info_builder_full() {
    let info = ModelInfo::builder()
        .id("claude-sonnet-5-20260601")
        .context_window(200_000)
        .max_output_tokens(32_000)
        .capabilities(
            ModelCapabilities::EXTENDED_THINKING
                | ModelCapabilities::PROMPT_CACHING
                | ModelCapabilities::STRUCTURED_OUTPUT
                | ModelCapabilities::TOOL_USE
                | ModelCapabilities::VISION,
        )
        .cache_ttls([CacheTtl::FiveMinutes])
        .build()
        .expect("build should succeed");
    assert_eq!(info.cache_ttl, vec![CacheTtl::FiveMinutes]);
}

#[test]
fn model_info_builder_missing_id_returns_none() {
    let result = ModelInfo::builder()
        .context_window(100_000)
        .max_output_tokens(8_000)
        .build();
    assert!(result.is_none());
}

#[test]
fn model_info_builder_missing_context_window_returns_none() {
    let result = ModelInfo::builder()
        .id("custom-model-v1")
        .max_output_tokens(8_000)
        .build();
    assert!(result.is_none());
}
