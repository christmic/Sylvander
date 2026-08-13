//! Translation between Agent prompt evidence and the public Session contract.
//!
//! Agent owns prompt composition semantics; Protocol owns the durable and wire
//! representation exposed to clients. Runtime is the only layer allowed to
//! join them, so neither lower crate depends on the other.

use sylvander_agent::prompt::{
    PromptLayerKind as AgentPromptLayerKind, PromptManifest as AgentPromptManifest,
    PromptModelSelection, PromptValidationIssue, validate_profile_selectors,
};
use sylvander_api::{ModelSelection, PromptLayerDigest, PromptLayerKind, PromptManifest};

/// Project a public model selection into the minimal Agent prompt input.
#[must_use]
pub fn agent_model_selection(selection: &ModelSelection) -> PromptModelSelection {
    PromptModelSelection {
        provider_id: selection.provider_id.clone(),
        model_id: selection.model_id.clone(),
    }
}

pub(crate) fn validate_public_prompt_selectors(
    selections: &[ModelSelection],
) -> Result<(), PromptValidationIssue> {
    let selections = selections
        .iter()
        .map(agent_model_selection)
        .collect::<Vec<_>>();
    validate_profile_selectors(&selections)
}

/// Project Agent-owned prompt evidence into the durable public representation.
#[must_use]
pub fn public_prompt_manifest(manifest: AgentPromptManifest) -> PromptManifest {
    PromptManifest {
        layers: manifest
            .layers
            .into_iter()
            .map(|layer| PromptLayerDigest {
                kind: match layer.kind {
                    AgentPromptLayerKind::SharedSafety => PromptLayerKind::SharedSafety,
                    AgentPromptLayerKind::ProviderModelProfile => {
                        PromptLayerKind::ProviderModelProfile
                    }
                    AgentPromptLayerKind::Agent => PromptLayerKind::Agent,
                    AgentPromptLayerKind::SessionInput => PromptLayerKind::SessionInput,
                },
                reference: layer.reference,
                sha256: layer.sha256,
                byte_count: layer.byte_count,
            })
            .collect(),
        aggregate_sha256: manifest.aggregate_sha256,
        total_bytes: manifest.total_bytes,
    }
}
