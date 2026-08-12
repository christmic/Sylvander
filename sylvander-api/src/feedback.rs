//! Evidence-bound user feedback DTOs.
//!
//! Feedback targets are opaque Runtime-issued handles. Public clients can
//! assess a turn without learning internal run or turn identifiers.

use serde::{Deserialize, Serialize};

/// A user assessment tied to durable execution evidence, never free-floating
/// training data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackRating {
    Positive,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackTaskResult {
    Succeeded,
    Failed,
    Partial,
    Cancelled,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackPrivacyClass {
    MetadataOnly,
    #[default]
    Private,
    Shareable,
}

/// Opaque, server-issued handle for one durable execution turn.
///
/// Clients must preserve this value verbatim. The wire contract deliberately
/// does not expose Runtime run or turn identifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct FeedbackTarget(pub String);

impl FeedbackTarget {
    /// Return whether this value has the exact server-issued digest shape.
    ///
    /// This validates framing only; Runtime must still resolve the target and
    /// authorize the owning session before accepting feedback.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        self.0.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReference {
    pub locator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunFeedback {
    pub target: FeedbackTarget,
    pub rating: FeedbackRating,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_result: Option<FeedbackTaskResult>,
    #[serde(default)]
    pub artifacts: Vec<EvidenceReference>,
    #[serde(default)]
    pub validations: Vec<EvidenceReference>,
    #[serde(default)]
    pub privacy_class: FeedbackPrivacyClass,
}

#[cfg(test)]
#[path = "../tests/unit/feedback.rs"]
mod tests;
