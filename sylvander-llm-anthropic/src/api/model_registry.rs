//! Hardcoded model metadata.
//!
//! v2 does **not** call `GET /v1/models` — model information is
//! statically encoded here so the Agent loop can make decisions
//! (compression thresholds, capability checks, cost estimation) without
//! an extra round-trip on startup.

use super::types::CacheTtl;

/// Identifies a model supported by Sylvander v2.
///
/// Wire format is the canonical Anthropic model ID string — see
/// [`ModelId::as_str`]. The enum itself is not serialized; callers
/// should use [`ModelId::as_str`] when building a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelId {
    /// Claude Sonnet 5 (`claude-sonnet-5-20260601`).
    ClaudeSonnet5,
    /// Claude Opus 5 (`claude-opus-5-20260601`).
    ClaudeOpus5,
}

impl ModelId {
    /// Canonical Anthropic model ID string used on the wire.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::ClaudeSonnet5 => "claude-sonnet-5-20260601",
            Self::ClaudeOpus5 => "claude-opus-5-20260601",
        }
    }

    /// Get the metadata for this model.
    #[must_use]
    pub fn info(&self) -> &'static ModelInfo {
        match self {
            Self::ClaudeSonnet5 => &SONNET_5_INFO,
            Self::ClaudeOpus5 => &OPUS_5_INFO,
        }
    }
}

impl std::fmt::Display for ModelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Static metadata for a single model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInfo {
    /// Model ID enum value.
    pub id: ModelId,
    /// Total context window size (input + output) in tokens.
    pub context_window: u32,
    /// Maximum output tokens in a single response.
    pub max_output_tokens: u32,
    /// Capability flags — see [`ModelCapabilities`].
    pub capabilities: ModelCapabilities,
    /// Supported cache TTLs.
    pub cache_ttl: &'static [CacheTtl],
}

/// Bitflag-style capabilities. New capabilities should be added as new
/// flags; checking is `model.capabilities.contains(ModelCapabilities::X)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModelCapabilities(u8);

impl ModelCapabilities {
    /// Extended thinking support.
    pub const EXTENDED_THINKING: Self = Self(1 << 0);
    /// Prompt caching (`cache_control` breakpoints).
    pub const PROMPT_CACHING: Self = Self(1 << 1);
    /// Structured output (`output_config` with JSON schema).
    pub const STRUCTURED_OUTPUT: Self = Self(1 << 2);
    /// Custom function tool use.
    pub const TOOL_USE: Self = Self(1 << 3);

    /// Empty capability set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Combine two capability sets. Const-friendly so it works in
    /// `static` initializers.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `true` if `self` contains all the flags in `other`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// `true` if any flag in `other` is also set in `self`.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }

    /// Raw bitmask value (for serialization if ever needed).
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for ModelCapabilities {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for ModelCapabilities {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

// -----------------------------------------------------------------------------
// Hardcoded model metadata — sourced from Anthropic model documentation.
// These values are intentionally static; update them when Anthropic releases
// new versions or capability changes.
// -----------------------------------------------------------------------------

/// Claude Sonnet 5 metadata.
pub static SONNET_5_INFO: ModelInfo = ModelInfo {
    id: ModelId::ClaudeSonnet5,
    context_window: 200_000,
    max_output_tokens: 32_000,
    capabilities: ModelCapabilities::EXTENDED_THINKING
        .union(ModelCapabilities::PROMPT_CACHING)
        .union(ModelCapabilities::STRUCTURED_OUTPUT)
        .union(ModelCapabilities::TOOL_USE),
    cache_ttl: &[CacheTtl::FiveMinutes],
};

/// Claude Opus 5 metadata.
pub static OPUS_5_INFO: ModelInfo = ModelInfo {
    id: ModelId::ClaudeOpus5,
    context_window: 200_000,
    max_output_tokens: 32_000,
    capabilities: ModelCapabilities::EXTENDED_THINKING
        .union(ModelCapabilities::PROMPT_CACHING)
        .union(ModelCapabilities::STRUCTURED_OUTPUT)
        .union(ModelCapabilities::TOOL_USE),
    cache_ttl: &[CacheTtl::FiveMinutes, CacheTtl::OneHour],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sonnet5_wire_id() {
        assert_eq!(ModelId::ClaudeSonnet5.as_str(), "claude-sonnet-5-20260601");
    }

    #[test]
    fn opus5_supports_one_hour_cache() {
        let info = ModelId::ClaudeOpus5.info();
        assert!(info.cache_ttl.contains(&CacheTtl::OneHour));
    }

    #[test]
    fn sonnet5_only_supports_5m_cache() {
        let info = ModelId::ClaudeSonnet5.info();
        assert!(info.cache_ttl.contains(&CacheTtl::FiveMinutes));
        assert!(!info.cache_ttl.contains(&CacheTtl::OneHour));
    }

    #[test]
    fn both_models_have_full_capabilities() {
        for id in [ModelId::ClaudeSonnet5, ModelId::ClaudeOpus5] {
            let caps = id.info().capabilities;
            assert!(caps.contains(ModelCapabilities::EXTENDED_THINKING));
            assert!(caps.contains(ModelCapabilities::PROMPT_CACHING));
            assert!(caps.contains(ModelCapabilities::STRUCTURED_OUTPUT));
            assert!(caps.contains(ModelCapabilities::TOOL_USE));
        }
    }

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
    fn display_uses_wire_id() {
        assert_eq!(
            format!("{}", ModelId::ClaudeSonnet5),
            "claude-sonnet-5-20260601"
        );
    }
}