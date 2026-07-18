//! Qualified model identity and capabilities.

use serde::{Deserialize, Serialize};

/// Provider-qualified model identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    /// Runtime provider registry identifier.
    pub provider: String,
    /// Model identifier within that provider.
    pub model: String,
}

impl ModelRef {
    #[must_use]
    /// Construct a provider-qualified model reference.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
        }
    }
}

/// Provider-neutral capability bitset advertised for one model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelCapabilities(u16);

impl ModelCapabilities {
    /// Model accepts an explicit reasoning budget and reasoning history.
    pub const REASONING: Self = Self(1 << 0);
    /// Model/provider supports prompt cache hints.
    pub const PROMPT_CACHING: Self = Self(1 << 1);
    /// Model supports schema-constrained output.
    pub const STRUCTURED_OUTPUT: Self = Self(1 << 2);
    /// Model accepts tool definitions, calls, and results.
    pub const TOOL_USE: Self = Self(1 << 3);
    /// Model accepts image input.
    pub const VISION: Self = Self(1 << 4);
    /// Model accepts document input.
    pub const DOCUMENT_INPUT: Self = Self(1 << 5);

    #[must_use]
    /// Return a capability set with no enabled features.
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    /// Return whether every bit in `other` is enabled.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    /// Return the union of two capability sets.
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

/// Catalog metadata used to validate and size model requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Provider-qualified identity.
    pub reference: ModelRef,
    /// Maximum combined context size advertised by the provider.
    pub context_window: u32,
    /// Maximum model output size.
    pub max_output_tokens: u32,
    /// Features implemented by this model/provider pair.
    pub capabilities: ModelCapabilities,
}
