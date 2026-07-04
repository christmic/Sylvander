//! Token usage accounting.

use serde::{Deserialize, Serialize};

/// Token usage returned by the Messages API. Reported on every successful
/// response (sync) and accumulated in `message_delta` events (streaming).
///
/// The total billable input tokens for a request is the sum of
/// `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens billed at full price.
    pub input_tokens: u32,
    /// Output tokens generated.
    pub output_tokens: u32,
    /// Input tokens used to create new cache entries (billed at cache-write
    /// rate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u32>,
    /// Input tokens read from a cache hit (billed at cache-read rate,
    /// typically 10% of full price).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u32>,
}

impl Usage {
    /// Total input tokens billed for this request (input + cache creation +
    /// cache read).
    #[must_use]
    pub fn total_input_tokens(&self) -> u32 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens.unwrap_or(0))
            .saturating_add(self.cache_read_input_tokens.unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_minimal() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        assert_eq!(
            serde_json::to_string(&usage).unwrap(),
            r#"{"input_tokens":100,"output_tokens":50}"#
        );
    }

    #[test]
    fn serializes_full() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(1024),
            cache_read_input_tokens: Some(4096),
        };
        let json = serde_json::to_string(&usage).unwrap();
        let back: Usage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, usage);
    }

    #[test]
    fn deserializes_minimal_from_anthropic() {
        let json = r#"{"input_tokens":42,"output_tokens":7}"#;
        let usage: Usage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, 42);
        assert_eq!(usage.output_tokens, 7);
        assert_eq!(usage.cache_creation_input_tokens, None);
        assert_eq!(usage.cache_read_input_tokens, None);
    }

    #[test]
    fn total_input_tokens_sums_all() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: Some(1024),
            cache_read_input_tokens: Some(4096),
        };
        assert_eq!(usage.total_input_tokens(), 5220);
    }
}