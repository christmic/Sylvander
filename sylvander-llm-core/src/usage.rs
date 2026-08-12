//! Provider-neutral token accounting.

use serde::{Deserialize, Serialize};

/// Provider-reported token dimensions that refine the input/output totals.
///
/// A missing value means the selected protocol did not report that dimension;
/// it is intentionally different from a reported zero. Output detail fields
/// are subsets of `TokenUsage::output_tokens` and must not be added to it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsageDetails {
    /// Provider-reported total across input and output tokens, when supplied.
    pub reported_total_tokens: Option<u64>,
    /// Tokens used to create an Anthropic five-minute cache entry.
    pub cache_write_5m_tokens: Option<u64>,
    /// Tokens used to create an Anthropic one-hour cache entry.
    pub cache_write_1h_tokens: Option<u64>,
    /// Output tokens spent on model reasoning or thinking.
    pub reasoning_tokens: Option<u64>,
    /// Input audio tokens included in the input total.
    pub audio_input_tokens: Option<u64>,
    /// Output audio tokens included in the output total.
    pub audio_output_tokens: Option<u64>,
    /// Predicted output tokens accepted by the model.
    pub accepted_prediction_tokens: Option<u64>,
    /// Predicted output tokens rejected by the model.
    pub rejected_prediction_tokens: Option<u64>,
}

/// Token accounting dimensions reported for one or more invocations.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Non-cache input tokens.
    pub input_tokens: u64,
    /// Generated output tokens.
    pub output_tokens: u64,
    /// Cache-write tokens reported by the provider. `None` means the
    /// provider did not report this dimension; it is distinct from zero.
    pub cache_write_tokens: Option<u64>,
    /// Cache-read tokens reported by the provider. `None` means the provider
    /// did not report this dimension; it is distinct from zero.
    pub cache_read_tokens: Option<u64>,
    /// Optional protocol detail represented by typed, provider-neutral fields.
    #[serde(default)]
    pub details: TokenUsageDetails,
}

impl TokenUsage {
    /// Add another usage record without integer overflow.
    pub fn saturating_add_assign(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_write_tokens = add_optional(self.cache_write_tokens, other.cache_write_tokens);
        self.cache_read_tokens = add_optional(self.cache_read_tokens, other.cache_read_tokens);
        self.details.saturating_add_assign(other.details);
    }

    #[must_use]
    /// Return all input-side dimensions, treating unreported caches as zero.
    pub fn total_input_tokens(self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_write_tokens.unwrap_or(0))
            .saturating_add(self.cache_read_tokens.unwrap_or(0))
    }
}

impl TokenUsageDetails {
    /// Add another detail record without integer overflow.
    pub fn saturating_add_assign(&mut self, other: Self) {
        self.cache_write_5m_tokens =
            add_optional(self.cache_write_5m_tokens, other.cache_write_5m_tokens);
        self.cache_write_1h_tokens =
            add_optional(self.cache_write_1h_tokens, other.cache_write_1h_tokens);
        self.reasoning_tokens = add_optional(self.reasoning_tokens, other.reasoning_tokens);
        self.audio_input_tokens = add_optional(self.audio_input_tokens, other.audio_input_tokens);
        self.audio_output_tokens =
            add_optional(self.audio_output_tokens, other.audio_output_tokens);
        self.accepted_prediction_tokens = add_optional(
            self.accepted_prediction_tokens,
            other.accepted_prediction_tokens,
        );
        self.rejected_prediction_tokens = add_optional(
            self.rejected_prediction_tokens,
            other.rejected_prediction_tokens,
        );
        self.reported_total_tokens =
            add_optional(self.reported_total_tokens, other.reported_total_tokens);
    }
}

fn add_optional(total: Option<u64>, next: Option<u64>) -> Option<u64> {
    match (total, next) {
        (None, None) => None,
        (total, next) => Some(total.unwrap_or(0).saturating_add(next.unwrap_or(0))),
    }
}
