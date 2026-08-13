//! Content-free evidence contract for governed internal cognition activation.

use serde::{Deserialize, Serialize};

/// Paired benchmark evidence and the exact policy it satisfied.
///
/// This DTO contains no prompts, responses, media, credentials, or mutable
/// activation state. Runtime independently validates every threshold before
/// an owner may approve the corresponding Registry fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CognitionActivationEvidence {
    pub evidence_set_sha256: String,
    pub pairs: u32,
    pub minimum_pairs: u32,
    pub unsafe_candidates: u32,
    pub median_reward_gain_micros: i64,
    pub minimum_reward_gain_micros: i64,
    pub quality_win_basis_points: u16,
    pub minimum_quality_win_basis_points: u16,
    pub median_token_increase_basis_points: i32,
    pub maximum_token_increase_basis_points: u16,
    pub p95_latency_increase_basis_points: i32,
    pub maximum_p95_latency_increase_basis_points: u16,
}
