//! Provider adapter contract tests.

use serde_json::json;

use super::*;

#[test]
fn model_capability_table_round_trips() {
    for (anthropic_capability, core_capability) in capability_pairs() {
        let anthropic = anthropic_model::ModelInfo {
            id: "claude-test".into(),
            context_window: 100,
            max_output_tokens: 10,
            capabilities: anthropic_capability,
            cache_ttl: Vec::new(),
        };
        let core = model_to_core(ANTHROPIC, &anthropic).unwrap();
        assert!(core.capabilities.contains(core_capability));
        assert_eq!(model_from_core(&core).unwrap(), anthropic);
    }
}

#[test]
fn tool_cache_table_is_lossless_or_rejected() {
    for cache_hint in [None, Some(core::CacheHint::Ephemeral)] {
        let tools = vec![core::ToolDefinition {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
            cache_hint,
        }];
        assert_eq!(tools_to_core(&tools_from_core(&tools)).unwrap(), tools);
    }

    let mut tool = tools_from_core(&[core::ToolDefinition {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: json!({"type": "object"}),
        cache_hint: None,
    }])
    .remove(0);
    tool.cache_control = Some(anthropic_wire::CacheControl::new(
        anthropic_wire::CacheTtl::OneHour,
    ));
    assert_eq!(
        tools_to_core(&[tool]),
        Err(ProviderAdapterError::Unsupported("tool cache TTL"))
    );
}
