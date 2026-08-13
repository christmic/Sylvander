//! Governed internal model roles for one first-class Agent.
//!
//! Cognitive roles do not own tasks, mailboxes, memory, capabilities, or
//! topology edges. They are bounded helpers selected inside one Agent turn;
//! the primary model remains responsible for the model-visible answer.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sylvander_api::ModelSelection;

const DEFAULT_MAX_AUXILIARY_CALLS: u8 = 2;

/// A bounded purpose served by an auxiliary model inside one Agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitiveRole {
    FastDraft,
    Deliberation,
    Critic,
    Vision,
    Audio,
}

/// Exact model bound to one internal cognitive role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CognitiveRoleBinding {
    pub role: CognitiveRole,
    pub model: ModelSelection,
}

/// Optional internal cognition profile. An empty role set preserves the
/// existing single-model strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CognitionConfig {
    #[serde(default)]
    pub roles: Vec<CognitiveRoleBinding>,
    #[serde(default = "default_max_auxiliary_calls")]
    pub max_auxiliary_calls: u8,
}

const fn default_max_auxiliary_calls() -> u8 {
    DEFAULT_MAX_AUXILIARY_CALLS
}

impl Default for CognitionConfig {
    fn default() -> Self {
        Self {
            roles: Vec::new(),
            max_auxiliary_calls: DEFAULT_MAX_AUXILIARY_CALLS,
        }
    }
}

impl CognitionConfig {
    /// Validate that roles are unique and cannot escape the Agent allowlist.
    pub fn validate(&self, allowed: &[ModelSelection]) -> Result<(), CognitionConfigError> {
        if self.max_auxiliary_calls == 0 {
            return Err(CognitionConfigError::ZeroCallBudget);
        }
        let mut roles = HashSet::with_capacity(self.roles.len());
        for binding in &self.roles {
            if !roles.insert(binding.role) {
                return Err(CognitionConfigError::DuplicateRole(binding.role));
            }
            if !allowed.contains(&binding.model) {
                return Err(CognitionConfigError::ModelOutsideAllowlist(
                    binding.model.clone(),
                ));
            }
        }
        Ok(())
    }

    fn binding(&self, role: CognitiveRole) -> Option<&CognitiveRoleBinding> {
        self.roles.iter().find(|binding| binding.role == role)
    }

    /// Produce a deterministic, content-free auxiliary plan. Hard modality
    /// and safety needs are selected before efficiency heuristics.
    #[must_use]
    pub fn plan(&self, signals: CognitiveSignals) -> CognitivePlan {
        let mut selected = Vec::new();
        let capacity = usize::from(self.max_auxiliary_calls);
        let mut include = |role| {
            if selected.len() < capacity
                && let Some(binding) = self.binding(role)
            {
                selected.push(binding.clone());
            }
        };

        match signals.modality {
            InputModality::Vision => include(CognitiveRole::Vision),
            InputModality::Audio => include(CognitiveRole::Audio),
            InputModality::Text => {}
        }
        if signals.risk == CognitiveRisk::High || signals.prior_failures > 0 {
            include(CognitiveRole::Critic);
        }
        if signals.complexity >= 70 || signals.uncertainty >= 60 {
            include(CognitiveRole::Deliberation);
        } else if signals.complexity <= 30 && signals.risk == CognitiveRisk::Low {
            include(CognitiveRole::FastDraft);
        }

        CognitivePlan {
            auxiliary: selected,
            primary_is_final_authority: true,
        }
    }
}

/// Content-free turn signals supplied by a Runtime-owned classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CognitiveSignals {
    pub modality: InputModality,
    /// Normalized `0..=100` estimate.
    pub complexity: u8,
    /// Normalized `0..=100` estimate.
    pub uncertainty: u8,
    pub risk: CognitiveRisk,
    pub prior_failures: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputModality {
    Text,
    Vision,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CognitiveRisk {
    Low,
    Medium,
    High,
}

/// Internal calls selected before the authoritative primary call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CognitivePlan {
    pub auxiliary: Vec<CognitiveRoleBinding>,
    pub primary_is_final_authority: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CognitionConfigError {
    #[error("cognition auxiliary-call budget must be greater than zero")]
    ZeroCallBudget,
    #[error("cognitive role {0:?} is configured more than once")]
    DuplicateRole(CognitiveRole),
    #[error("cognitive model {0:?} is outside the Agent model allowlist")]
    ModelOutsideAllowlist(ModelSelection),
}

#[cfg(test)]
#[path = "../../tests/unit/agent_cognition.rs"]
mod tests;
