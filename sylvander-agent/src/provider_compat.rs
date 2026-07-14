//! Lossless model and tool compatibility at the legacy Anthropic boundary.

use sylvander_llm_anthropic::api::{model as legacy_model, types as legacy};
use sylvander_llm_core as core;
use thiserror::Error;

mod content;

pub use content::{message_from_core, message_to_core, response_from_core};

const ANTHROPIC: &str = "anthropic";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderCompatError {
    #[error("unsupported lossless conversion: {0}")]
    Unsupported(&'static str),
    #[error("expected provider {expected}, got {actual}")]
    ProviderMismatch { expected: String, actual: String },
    #[error("{field} value {value} exceeds the legacy u32 limit")]
    NumericOverflow { field: &'static str, value: u64 },
}

pub fn model_to_core(
    provider: &str,
    model: &legacy_model::ModelInfo,
) -> Result<core::ModelInfo, ProviderCompatError> {
    if !model.cache_ttl.is_empty() {
        return Err(ProviderCompatError::Unsupported("model cache TTL metadata"));
    }
    Ok(core::ModelInfo {
        reference: core::ModelRef::new(provider, &model.id),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities: capabilities_to_core(model.capabilities),
    })
}

pub fn model_from_core(
    model: &core::ModelInfo,
) -> Result<legacy_model::ModelInfo, ProviderCompatError> {
    require_anthropic(&model.reference.provider)?;
    Ok(legacy_model::ModelInfo {
        id: model.reference.model.clone(),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities: capabilities_from_core(model.capabilities),
        cache_ttl: Vec::new(),
    })
}

pub fn tools_to_core(
    tools: &[legacy::Tool],
) -> Result<Vec<core::ToolDefinition>, ProviderCompatError> {
    tools
        .iter()
        .map(|tool| {
            let cache_hint = match tool.cache_control {
                None => None,
                Some(control) if control.ttl.is_none() => Some(core::CacheHint::Ephemeral),
                Some(_) => return Err(ProviderCompatError::Unsupported("tool cache TTL")),
            };
            Ok(core::ToolDefinition {
                name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: tool.input_schema.schema.clone(),
                cache_hint,
            })
        })
        .collect()
}

#[must_use]
pub fn tools_from_core(tools: &[core::ToolDefinition]) -> Vec<legacy::Tool> {
    tools
        .iter()
        .map(|tool| legacy::Tool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: legacy::InputSchema::from_json_value(tool.input_schema.clone()),
            cache_control: tool.cache_hint.map(|_| legacy::CacheControl::ephemeral()),
        })
        .collect()
}

fn require_anthropic(provider: &str) -> Result<(), ProviderCompatError> {
    if provider == ANTHROPIC {
        Ok(())
    } else {
        Err(ProviderCompatError::ProviderMismatch {
            expected: ANTHROPIC.into(),
            actual: provider.into(),
        })
    }
}

fn capabilities_to_core(value: legacy_model::ModelCapabilities) -> core::ModelCapabilities {
    capability_pairs()
        .into_iter()
        .fold(core::ModelCapabilities::empty(), |all, (legacy, core)| {
            if value.contains(legacy) {
                all | core
            } else {
                all
            }
        })
}

fn capabilities_from_core(value: core::ModelCapabilities) -> legacy_model::ModelCapabilities {
    capability_pairs().into_iter().fold(
        legacy_model::ModelCapabilities::empty(),
        |all, (legacy, core)| {
            if value.contains(core) {
                all | legacy
            } else {
                all
            }
        },
    )
}

fn capability_pairs() -> [(legacy_model::ModelCapabilities, core::ModelCapabilities); 6] {
    [
        (
            legacy_model::ModelCapabilities::EXTENDED_THINKING,
            core::ModelCapabilities::REASONING,
        ),
        (
            legacy_model::ModelCapabilities::PROMPT_CACHING,
            core::ModelCapabilities::PROMPT_CACHING,
        ),
        (
            legacy_model::ModelCapabilities::STRUCTURED_OUTPUT,
            core::ModelCapabilities::STRUCTURED_OUTPUT,
        ),
        (
            legacy_model::ModelCapabilities::TOOL_USE,
            core::ModelCapabilities::TOOL_USE,
        ),
        (
            legacy_model::ModelCapabilities::VISION,
            core::ModelCapabilities::VISION,
        ),
        (
            legacy_model::ModelCapabilities::DOCUMENT_INPUT,
            core::ModelCapabilities::DOCUMENT_INPUT,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn model_capability_table_round_trips() {
        for (legacy_capability, core_capability) in capability_pairs() {
            let legacy = legacy_model::ModelInfo {
                id: "claude-test".into(),
                context_window: 100,
                max_output_tokens: 10,
                capabilities: legacy_capability,
                cache_ttl: Vec::new(),
            };
            let core = model_to_core(ANTHROPIC, &legacy).unwrap();
            assert!(core.capabilities.contains(core_capability));
            assert_eq!(model_from_core(&core).unwrap(), legacy);
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
        tool.cache_control = Some(legacy::CacheControl::new(legacy::CacheTtl::OneHour));
        assert_eq!(
            tools_to_core(&[tool]),
            Err(ProviderCompatError::Unsupported("tool cache TTL"))
        );
    }
}
