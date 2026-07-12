//! Model metadata types.
//!
//! v2 SDK provides the **shape** of model metadata (`ModelInfo`,
//! `ModelCapabilities`) but does **not** populate any specific model
//! entries — that's the caller's responsibility. Different
//! deployments/backends have different model catalogs (e.g., Anthropic
//! direct has `claude-sonnet-5-20260601`, a custom proxy might expose
//! different IDs).
//!
//! M2+ layers (e.g., a `sylvander-core` crate or the Agent Loop) are
//! expected to maintain their own `HashMap<String, ModelInfo>` registries.
//! This crate just provides the data types so those registries can be
//! expressed in a consistent shape.

use super::types::CacheTtl;

/// Bitflag-style capability flags for a model.
///
/// The `ModelCapabilities::empty()` / `union()` / `contains()` API lets
/// callers express combinations without depending on specific model
/// values. The SDK does NOT define which models have which capabilities
/// — that knowledge lives in caller-built registries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
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
    /// Vision / image input support.
    pub const VISION: Self = Self(1 << 4);
    /// Document (PDF) input support.
    pub const DOCUMENT_INPUT: Self = Self(1 << 5);

    /// Empty capability set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Combine two capability sets. Const-friendly so it works in
    /// `static` initializers and `const fn` constructors.
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

    /// Raw bitmask value. Exposed for callers that want to serialize
    /// capabilities into their own registries.
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

/// Metadata describing a single model. Callers construct `ModelInfo`
/// values to populate their own model registries — the SDK does not
/// ship any pre-filled entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    /// Canonical Anthropic model ID string (e.g.,
    /// `"claude-sonnet-5-20260601"`). Free-form — the SDK treats it as
    /// an opaque identifier for wire requests.
    pub id: String,
    /// Total context window size (input + output) in tokens.
    pub context_window: u32,
    /// Maximum output tokens in a single response.
    pub max_output_tokens: u32,
    /// Capability flags. See [`ModelCapabilities`].
    pub capabilities: ModelCapabilities,
    /// Supported cache TTLs for this model.
    pub cache_ttl: Vec<CacheTtl>,
}

impl ModelInfo {
    /// Start building a `ModelInfo`. Used by callers to populate their
    /// own model registries.
    #[must_use]
    pub fn builder() -> ModelInfoBuilder {
        ModelInfoBuilder::default()
    }
}

/// Builder for [`ModelInfo`].
#[derive(Debug, Default)]
pub struct ModelInfoBuilder {
    id: Option<String>,
    context_window: Option<u32>,
    max_output_tokens: Option<u32>,
    capabilities: ModelCapabilities,
    cache_ttl: Vec<CacheTtl>,
}

impl ModelInfoBuilder {
    /// Set the model ID (canonical Anthropic string).
    #[must_use]
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Set the context window size in tokens.
    #[must_use]
    pub fn context_window(mut self, n: u32) -> Self {
        self.context_window = Some(n);
        self
    }

    /// Set the maximum output tokens per response.
    #[must_use]
    pub fn max_output_tokens(mut self, n: u32) -> Self {
        self.max_output_tokens = Some(n);
        self
    }

    /// Add a capability flag.
    #[must_use]
    pub fn capability(mut self, cap: ModelCapabilities) -> Self {
        self.capabilities = self.capabilities.union(cap);
        self
    }

    /// Set all capability flags at once.
    #[must_use]
    pub fn capabilities(mut self, caps: ModelCapabilities) -> Self {
        self.capabilities = caps;
        self
    }

    /// Add a supported cache TTL.
    #[must_use]
    pub fn cache_ttl(mut self, ttl: CacheTtl) -> Self {
        self.cache_ttl.push(ttl);
        self
    }

    /// Set all supported cache TTLs at once.
    #[must_use]
    pub fn cache_ttls(mut self, ttls: impl IntoIterator<Item = CacheTtl>) -> Self {
        self.cache_ttl = ttls.into_iter().collect();
        self
    }

    /// Build the `ModelInfo`.
    ///
    /// # Errors
    /// Returns `None` if `id`, `context_window`, or `max_output_tokens`
    /// is missing.
    #[must_use]
    pub fn build(self) -> Option<ModelInfo> {
        Some(ModelInfo {
            id: self.id?,
            context_window: self.context_window?,
            max_output_tokens: self.max_output_tokens?,
            capabilities: self.capabilities,
            cache_ttl: self.cache_ttl,
        })
    }
}

#[cfg(test)]
mod tests {
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
}
