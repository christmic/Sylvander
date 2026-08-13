//! Content-free routing contract for built-in Agent perception.
//!
//! This planner never decodes media or invokes a model. Runtime first proves
//! transport and primary-model capability, then may select one configured
//! specialist candidate. Skills consume governed perception artifacts later;
//! they do not become the transport or capability boundary.

use serde::{Deserialize, Serialize};
use sylvander_llm_core::ModelCapabilities;

use super::cognition::{CognitionConfig, CognitiveRole, CognitiveRoleBinding};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionModality {
    Image,
    Audio,
    Document,
}

/// Facts resolved before any media leaves the Runtime boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerceptionSignals {
    pub modality: PerceptionModality,
    /// Whether the current Channel and provider-neutral content contract can
    /// carry this modality. Audio is currently false until a typed audio block
    /// exists end to end.
    pub transport_supported: bool,
    pub primary_capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PerceptionUnavailableReason {
    TransportUnsupported,
    NoCapableRoute,
}

/// Deterministic route proposal. A specialist remains the same Agent's
/// internal dependency and gains no tools, mailbox, memory, or workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum PerceptionPlan {
    NativePrimary,
    SpecialistCandidate { binding: CognitiveRoleBinding },
    Unavailable { reason: PerceptionUnavailableReason },
}

#[must_use]
pub fn plan_perception(cognition: &CognitionConfig, signals: PerceptionSignals) -> PerceptionPlan {
    if !signals.transport_supported {
        return PerceptionPlan::Unavailable {
            reason: PerceptionUnavailableReason::TransportUnsupported,
        };
    }
    let (native_capability, specialist_role) = match signals.modality {
        PerceptionModality::Image => (Some(ModelCapabilities::VISION), CognitiveRole::Vision),
        PerceptionModality::Document => (
            Some(ModelCapabilities::DOCUMENT_INPUT),
            CognitiveRole::Document,
        ),
        PerceptionModality::Audio => (Some(ModelCapabilities::AUDIO_INPUT), CognitiveRole::Audio),
    };
    if native_capability.is_some_and(|capability| signals.primary_capabilities.contains(capability))
    {
        return PerceptionPlan::NativePrimary;
    }
    cognition.binding(specialist_role).map_or(
        PerceptionPlan::Unavailable {
            reason: PerceptionUnavailableReason::NoCapableRoute,
        },
        |binding| PerceptionPlan::SpecialistCandidate {
            binding: binding.clone(),
        },
    )
}

#[cfg(test)]
#[path = "../../tests/unit/agent_perception.rs"]
mod tests;
