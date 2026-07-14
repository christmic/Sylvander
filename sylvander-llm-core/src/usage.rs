//! Provider-neutral token accounting.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_read_tokens: u64,
}

impl TokenUsage {
    pub fn saturating_add_assign(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
    }

    #[must_use]
    pub const fn total_input_tokens(self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_write_tokens)
            .saturating_add(self.cache_read_tokens)
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
            cache_write_tokens: 3,
            cache_read_tokens: 4,
        };
        total.saturating_add_assign(TokenUsage {
            input_tokens: 1,
            output_tokens: 5,
            cache_write_tokens: 7,
            cache_read_tokens: 11,
        });
        assert_eq!(total.input_tokens, u64::MAX);
        assert_eq!(total.output_tokens, 7);
        assert_eq!(total.cache_write_tokens, 10);
        assert_eq!(total.cache_read_tokens, 15);
        assert_eq!(total.total_input_tokens(), u64::MAX);
    }
}
