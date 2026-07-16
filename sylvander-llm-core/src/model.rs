//! Qualified model identity and capabilities.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl ModelRef {
    #[must_use]
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelCapabilities(u16);

impl ModelCapabilities {
    pub const REASONING: Self = Self(1 << 0);
    pub const PROMPT_CACHING: Self = Self(1 << 1);
    pub const STRUCTURED_OUTPUT: Self = Self(1 << 2);
    pub const TOOL_USE: Self = Self(1 << 3);
    pub const VISION: Self = Self(1 << 4);
    pub const DOCUMENT_INPUT: Self = Self(1 << 5);

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

impl std::ops::BitOr for ModelCapabilities {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub reference: ModelRef,
    pub context_window: u32,
    pub max_output_tokens: u32,
    pub capabilities: ModelCapabilities,
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn provider_is_part_of_model_identity() {
        let mut models = HashSet::new();
        models.insert(ModelRef::new("anthropic", "shared-name"));
        models.insert(ModelRef::new("local", "shared-name"));
        assert_eq!(models.len(), 2);
    }
}
