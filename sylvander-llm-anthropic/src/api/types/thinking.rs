//! Thinking configuration for the current Anthropic Messages protocol.

use serde::{Deserialize, Serialize};

/// Controls how thinking content is represented in the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingDisplay {
    /// Return summarized thinking blocks.
    Summarized,
    /// Omit readable thinking while retaining continuity information.
    Omitted,
}

/// Thinking mode sent in a Messages request.
///
/// This tagged union mirrors `ThinkingConfigParam` in the pinned official SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThinkingConfig {
    /// Use an explicit token budget.
    Enabled {
        /// Maximum tokens available to the reasoning process.
        budget_tokens: u32,
        /// Controls whether returned thinking is summarized or omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<ThinkingDisplay>,
    },
    /// Let a compatible model choose its thinking budget.
    Adaptive {
        /// Controls whether returned thinking is summarized or omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<ThinkingDisplay>,
    },
    /// Explicitly disable thinking.
    Disabled,
}

impl ThinkingConfig {
    /// Enable thinking with an explicit budget.
    #[must_use]
    pub const fn new(budget_tokens: u32) -> Self {
        Self::Enabled {
            budget_tokens,
            display: None,
        }
    }

    /// Enable adaptive thinking.
    #[must_use]
    pub const fn adaptive() -> Self {
        Self::Adaptive { display: None }
    }

    /// Return the explicit budget, if this is the enabled mode.
    #[must_use]
    pub const fn budget_tokens(self) -> Option<u32> {
        match self {
            Self::Enabled { budget_tokens, .. } => Some(budget_tokens),
            Self::Adaptive { .. } | Self::Disabled => None,
        }
    }

    /// Set the thinking display behavior for enabled or adaptive thinking.
    #[must_use]
    pub const fn with_display(self, display: ThinkingDisplay) -> Self {
        match self {
            Self::Enabled { budget_tokens, .. } => Self::Enabled {
                budget_tokens,
                display: Some(display),
            },
            Self::Adaptive { .. } => Self::Adaptive {
                display: Some(display),
            },
            Self::Disabled => Self::Disabled,
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/api_types_thinking.rs"]
mod tests;
