//! Provider-neutral token accounting.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Cache-write tokens reported by the provider. `None` means the
    /// provider did not report this dimension; it is distinct from zero.
    pub cache_write_tokens: Option<u64>,
    /// Cache-read tokens reported by the provider. `None` means the provider
    /// did not report this dimension; it is distinct from zero.
    pub cache_read_tokens: Option<u64>,
}

impl TokenUsage {
    pub fn saturating_add_assign(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_write_tokens = add_optional(self.cache_write_tokens, other.cache_write_tokens);
        self.cache_read_tokens = add_optional(self.cache_read_tokens, other.cache_read_tokens);
    }

    #[must_use]
    pub fn total_input_tokens(self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_write_tokens.unwrap_or(0))
            .saturating_add(self.cache_read_tokens.unwrap_or(0))
    }
}

fn add_optional(total: Option<u64>, next: Option<u64>) -> Option<u64> {
    match (total, next) {
        (None, None) => None,
        (total, next) => Some(total.unwrap_or(0).saturating_add(next.unwrap_or(0))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_accumulates_without_overflowing() {
        let mut total = TokenUsage {
            input_tokens: u64::MAX,
            output_tokens: 2,
            cache_write_tokens: None,
            cache_read_tokens: Some(u64::MAX),
        };
        total.saturating_add_assign(TokenUsage {
            input_tokens: 1,
            output_tokens: 5,
            cache_write_tokens: Some(7),
            cache_read_tokens: None,
        });
        assert_eq!(total.input_tokens, u64::MAX);
        assert_eq!(total.output_tokens, 7);
        assert_eq!(total.cache_write_tokens, Some(7));
        assert_eq!(total.cache_read_tokens, Some(u64::MAX));
        assert_eq!(total.total_input_tokens(), u64::MAX);

        let mut unknown = TokenUsage::default();
        unknown.saturating_add_assign(TokenUsage::default());
        assert_eq!(unknown.cache_write_tokens, None);
        assert_eq!(unknown.cache_read_tokens, None);
    }
}
