//! Lossless translation between the provider-neutral model contract and the
//! current Anthropic wire representation.
//!
//! This module is crate-private by design. Runtime composition enters through
//! an exact provider-qualified router; these conversions preserve the existing
//! Agent loop's Anthropic-shaped transcript and tool internals without exposing
//! a second production backend or fallback route.

use sylvander_llm_anthropic::api::{model as anthropic_model, types as anthropic_wire};
use sylvander_llm_core as core;
use thiserror::Error;

#[path = "provider_adapter/content.rs"]
mod content;

pub(crate) use content::{message_to_core, response_from_core};

const ANTHROPIC: &str = "anthropic";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum ProviderAdapterError {
    #[error("unsupported lossless conversion: {0}")]
    Unsupported(&'static str),
    #[error("expected provider {expected}, got {actual}")]
    ProviderMismatch { expected: String, actual: String },
    #[error("{field} value {value} exceeds the Anthropic wire u32 limit")]
    NumericOverflow { field: &'static str, value: u64 },
}

#[cfg(test)]
pub(crate) fn model_to_core(
    provider: &str,
    model: &anthropic_model::ModelInfo,
) -> Result<core::ModelInfo, ProviderAdapterError> {
    if !model.cache_ttl.is_empty() {
        return Err(ProviderAdapterError::Unsupported(
            "model cache TTL metadata",
        ));
    }
    Ok(core::ModelInfo {
        reference: core::ModelRef::new(provider, &model.id),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities: capabilities_to_core(model.capabilities),
    })
}

#[cfg(test)]
pub(crate) fn model_from_core(
    model: &core::ModelInfo,
) -> Result<anthropic_model::ModelInfo, ProviderAdapterError> {
    require_anthropic(&model.reference.provider)?;
    Ok(model_metadata_from_core(model))
}

/// Build Anthropic-shaped metadata for the loop's compression and tool-context
/// internals. The exact qualified identity remains authoritative in the
/// provider backend.
#[must_use]
pub(crate) fn model_metadata_from_core(model: &core::ModelInfo) -> anthropic_model::ModelInfo {
    anthropic_model::ModelInfo {
        id: model.reference.model.clone(),
        context_window: model.context_window,
        max_output_tokens: model.max_output_tokens,
        capabilities: capabilities_from_core(model.capabilities),
        cache_ttl: Vec::new(),
    }
}

pub(crate) fn tools_to_core(
    tools: &[anthropic_wire::Tool],
) -> Result<Vec<core::ToolDefinition>, ProviderAdapterError> {
    tools
        .iter()
        .map(|tool| {
            let cache_hint = match tool.cache_control {
                None => None,
                Some(control) if control.ttl.is_none() => Some(core::CacheHint::Ephemeral),
                Some(_) => return Err(ProviderAdapterError::Unsupported("tool cache TTL")),
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
#[cfg(test)]
pub(crate) fn tools_from_core(tools: &[core::ToolDefinition]) -> Vec<anthropic_wire::Tool> {
    tools
        .iter()
        .map(|tool| anthropic_wire::Tool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: anthropic_wire::InputSchema::from_json_value(tool.input_schema.clone()),
            cache_control: tool
                .cache_hint
                .map(|_| anthropic_wire::CacheControl::ephemeral()),
        })
        .collect()
}

fn require_anthropic(provider: &str) -> Result<(), ProviderAdapterError> {
    if provider == ANTHROPIC {
        Ok(())
    } else {
        Err(ProviderAdapterError::ProviderMismatch {
            expected: ANTHROPIC.into(),
            actual: provider.into(),
        })
    }
}

#[cfg(test)]
fn capabilities_to_core(value: anthropic_model::ModelCapabilities) -> core::ModelCapabilities {
    capability_pairs().into_iter().fold(
        core::ModelCapabilities::empty(),
        |all, (anthropic, core)| {
            if value.contains(anthropic) {
                all | core
            } else {
                all
            }
        },
    )
}

fn capabilities_from_core(value: core::ModelCapabilities) -> anthropic_model::ModelCapabilities {
    capability_pairs().into_iter().fold(
        anthropic_model::ModelCapabilities::empty(),
        |all, (anthropic, core)| {
            if value.contains(core) {
                all | anthropic
            } else {
                all
            }
        },
    )
}

fn capability_pairs() -> [(anthropic_model::ModelCapabilities, core::ModelCapabilities); 6] {
    [
        (
            anthropic_model::ModelCapabilities::EXTENDED_THINKING,
            core::ModelCapabilities::REASONING,
        ),
        (
            anthropic_model::ModelCapabilities::PROMPT_CACHING,
            core::ModelCapabilities::PROMPT_CACHING,
        ),
        (
            anthropic_model::ModelCapabilities::STRUCTURED_OUTPUT,
            core::ModelCapabilities::STRUCTURED_OUTPUT,
        ),
        (
            anthropic_model::ModelCapabilities::TOOL_USE,
            core::ModelCapabilities::TOOL_USE,
        ),
        (
            anthropic_model::ModelCapabilities::VISION,
            core::ModelCapabilities::VISION,
        ),
        (
            anthropic_model::ModelCapabilities::DOCUMENT_INPUT,
            core::ModelCapabilities::DOCUMENT_INPUT,
        ),
    ]
}

#[cfg(test)]
#[path = "../tests/unit/provider_adapter.rs"]
mod tests;
