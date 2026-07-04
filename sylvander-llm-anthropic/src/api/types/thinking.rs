//! Extended thinking configuration.

use serde::{Deserialize, Serialize};

/// Extended thinking configuration. Attach to a request to enable the
/// model's internal reasoning step.
///
/// Wire format (when enabled):
/// ```text
/// { "budget_tokens": 1024 }
/// ```
///
/// To disable thinking, omit the field from the request — there is no
/// "disabled" variant.
///
/// Requires the `extended-thinking-2025-01-01` beta header, which the
/// client adds automatically when this field is present.
///
/// See [Anthropic extended thinking docs](https://platform.claude.com/docs/en/build-with-claude/extended-thinking)
/// for budget sizing guidance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkingConfig {
    /// Maximum tokens the model may use for its internal reasoning.
    /// Larger budgets allow more thorough analysis of complex problems.
    pub budget_tokens: u32,
}

impl ThinkingConfig {
    /// Create a thinking config with the given budget.
    #[must_use]
    pub const fn new(budget_tokens: u32) -> Self {
        Self { budget_tokens }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_minimal() {
        let tc = ThinkingConfig::new(1024);
        assert_eq!(
            serde_json::to_string(&tc).unwrap(),
            r#"{"budget_tokens":1024}"#
        );
    }

    #[test]
    fn roundtrip() {
        let tc = ThinkingConfig::new(8192);
        let json = serde_json::to_string(&tc).unwrap();
        let back: ThinkingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tc);
    }
}