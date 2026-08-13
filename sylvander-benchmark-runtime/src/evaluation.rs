//! Deterministic paired evidence gate for auxiliary cognition activation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    BenchmarkModelRole, CognitionProfile, FailurePoint, RuntimeBenchError, RuntimeBenchResult,
    ScenarioFamily, TopologyProfile, WorkspaceProfile,
};

const BASIS_POINTS: u64 = 10_000;
const REWARD_SCALE: f64 = 1_000_000.0;

/// Policy for promoting one cognition profile from evaluation-only to an
/// automatic Runtime route. Safety is always a hard gate and cannot be relaxed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationGatePolicy {
    pub minimum_pairs: u32,
    pub minimum_reward_gain_micros: i64,
    pub minimum_quality_win_basis_points: u16,
    pub maximum_token_increase_basis_points: u16,
    pub maximum_p95_latency_increase_basis_points: u16,
}

impl ActivationGatePolicy {
    pub fn validate(self) -> Result<(), ActivationGateError> {
        if self.minimum_pairs == 0 || self.minimum_quality_win_basis_points > BASIS_POINTS as u16 {
            return Err(ActivationGateError::InvalidPolicy);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationDecision {
    KeepDisabled,
    Eligible,
}

/// Content-free evidence summary. The report names no prompt, response, media,
/// or user content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationGateReport {
    pub candidate: CognitionProfile,
    pub decision: ActivationDecision,
    pub pairs: u32,
    pub unsafe_candidates: u32,
    pub median_reward_gain_micros: i64,
    pub quality_win_basis_points: u16,
    pub median_token_increase_basis_points: i32,
    pub p95_latency_increase_basis_points: i32,
}

/// Compare exact scenario/run pairs. Unpaired samples and mismatched primary
/// models are errors rather than silently discarded observations.
pub fn evaluate_cognition_activation(
    baseline: &[RuntimeBenchResult],
    candidate: &[RuntimeBenchResult],
    candidate_profile: CognitionProfile,
    policy: ActivationGatePolicy,
) -> Result<ActivationGateReport, ActivationGateError> {
    policy.validate()?;
    if candidate_profile == CognitionProfile::PrimaryOnly
        || baseline.is_empty()
        || candidate.is_empty()
    {
        return Err(ActivationGateError::InvalidCandidate);
    }
    let baseline = indexed_results(baseline, CognitionProfile::PrimaryOnly)?;
    let candidate = indexed_results(candidate, candidate_profile)?;
    if baseline.len() != candidate.len() || baseline.keys().ne(candidate.keys()) {
        return Err(ActivationGateError::UnpairedEvidence);
    }

    let mut reward_gains = Vec::with_capacity(baseline.len());
    let mut token_deltas = Vec::with_capacity(baseline.len());
    let mut latency_deltas = Vec::with_capacity(baseline.len());
    let mut quality_wins = 0_u64;
    let mut unsafe_candidates = 0_u32;
    for (key, baseline_result) in &baseline {
        let candidate_result = candidate
            .get(key)
            .ok_or(ActivationGateError::UnpairedEvidence)?;
        if primary_model(baseline_result) != primary_model(candidate_result) {
            return Err(ActivationGateError::PrimaryModelMismatch);
        }
        let baseline_reward = reward_micros(baseline_result.verifier_reward)?;
        let candidate_reward = reward_micros(candidate_result.verifier_reward)?;
        let gain = candidate_reward.saturating_sub(baseline_reward);
        reward_gains.push(gain);
        if gain > 0 {
            quality_wins = quality_wins.saturating_add(1);
        }
        token_deltas.push(relative_delta_basis_points(
            baseline_result
                .input_tokens
                .saturating_add(baseline_result.output_tokens),
            candidate_result
                .input_tokens
                .saturating_add(candidate_result.output_tokens),
        )?);
        latency_deltas.push(relative_delta_basis_points(
            baseline_result.duration_millis,
            candidate_result.duration_millis,
        )?);
        if !baseline_result.is_runtime_safe() {
            return Err(ActivationGateError::UnsafeBaseline);
        }
        if !candidate_result.is_runtime_safe() {
            unsafe_candidates = unsafe_candidates.saturating_add(1);
        }
    }
    let pairs = u32::try_from(baseline.len()).map_err(|_| ActivationGateError::TooManyPairs)?;
    let median_reward_gain_micros = median_i64(&mut reward_gains);
    let median_token_increase_basis_points = median_i32(&mut token_deltas);
    let p95_latency_increase_basis_points = percentile_i32(&mut latency_deltas, 95);
    let quality_win_basis_points = basis_points(quality_wins, u64::from(pairs));
    let eligible = pairs >= policy.minimum_pairs
        && unsafe_candidates == 0
        && median_reward_gain_micros >= policy.minimum_reward_gain_micros
        && quality_win_basis_points >= policy.minimum_quality_win_basis_points
        && median_token_increase_basis_points
            <= i32::from(policy.maximum_token_increase_basis_points)
        && p95_latency_increase_basis_points
            <= i32::from(policy.maximum_p95_latency_increase_basis_points);
    Ok(ActivationGateReport {
        candidate: candidate_profile,
        decision: if eligible {
            ActivationDecision::Eligible
        } else {
            ActivationDecision::KeepDisabled
        },
        pairs,
        unsafe_candidates,
        median_reward_gain_micros,
        quality_win_basis_points,
        median_token_increase_basis_points,
        p95_latency_increase_basis_points,
    })
}

fn indexed_results(
    results: &[RuntimeBenchResult],
    expected: CognitionProfile,
) -> Result<BTreeMap<PairKey, &RuntimeBenchResult>, ActivationGateError> {
    let mut indexed = BTreeMap::new();
    for result in results {
        result
            .validate()
            .map_err(ActivationGateError::InvalidResult)?;
        if result.coordinate.cognition != expected {
            return Err(ActivationGateError::ProfileMismatch);
        }
        let key = PairKey::from(result);
        if indexed.insert(key, result).is_some() {
            return Err(ActivationGateError::DuplicatePair);
        }
    }
    Ok(indexed)
}

fn primary_model(result: &RuntimeBenchResult) -> Option<&str> {
    result
        .coordinate
        .models
        .iter()
        .find(|binding| binding.role == BenchmarkModelRole::Primary)
        .map(|binding| binding.model.as_str())
}

fn reward_micros(reward: Option<f64>) -> Result<i64, ActivationGateError> {
    let reward = reward.ok_or(ActivationGateError::MissingReward)?;
    if !(0.0..=1.0).contains(&reward) {
        return Err(ActivationGateError::InvalidReward);
    }
    Ok((reward * REWARD_SCALE).round() as i64)
}

fn relative_delta_basis_points(baseline: u64, candidate: u64) -> Result<i32, ActivationGateError> {
    if baseline == 0 {
        return Err(ActivationGateError::ZeroBaselineMetric);
    }
    let difference = i128::from(candidate).saturating_sub(i128::from(baseline));
    let scaled = difference.saturating_mul(i128::from(BASIS_POINTS));
    i32::try_from(scaled / i128::from(baseline)).map_err(|_| ActivationGateError::MetricOverflow)
}

fn median_i64(values: &mut [i64]) -> i64 {
    values.sort_unstable();
    values[(values.len() - 1) / 2]
}

fn median_i32(values: &mut [i32]) -> i32 {
    values.sort_unstable();
    values[(values.len() - 1) / 2]
}

fn percentile_i32(values: &mut [i32], percentile: usize) -> i32 {
    values.sort_unstable();
    let rank = values.len().saturating_mul(percentile).saturating_add(99) / 100;
    values[rank.saturating_sub(1).min(values.len() - 1)]
}

fn basis_points(numerator: u64, denominator: u64) -> u16 {
    u16::try_from(numerator.saturating_mul(BASIS_POINTS) / denominator).unwrap_or(u16::MAX)
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PairKey {
    suite: String,
    suite_revision: String,
    scenario_id: String,
    family: ScenarioFamily,
    topology: TopologyProfile,
    workspace: WorkspaceProfile,
    failure_point: FailurePoint,
    run_ordinal: u32,
}

impl From<&RuntimeBenchResult> for PairKey {
    fn from(result: &RuntimeBenchResult) -> Self {
        Self {
            suite: result.coordinate.suite.clone(),
            suite_revision: result.coordinate.suite_revision.clone(),
            scenario_id: result.coordinate.scenario_id.clone(),
            family: result.coordinate.family,
            topology: result.coordinate.topology,
            workspace: result.coordinate.workspace,
            failure_point: result.coordinate.failure_point,
            run_ordinal: result.coordinate.run_ordinal,
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ActivationGateError {
    #[error("activation policy is invalid")]
    InvalidPolicy,
    #[error("activation candidate is invalid")]
    InvalidCandidate,
    #[error("benchmark result is invalid: {0}")]
    InvalidResult(RuntimeBenchError),
    #[error("benchmark cognition profile does not match the evidence group")]
    ProfileMismatch,
    #[error("paired benchmark evidence is incomplete")]
    UnpairedEvidence,
    #[error("paired benchmark coordinate is duplicated")]
    DuplicatePair,
    #[error("paired benchmark primary models differ")]
    PrimaryModelMismatch,
    #[error("paired verifier reward is missing")]
    MissingReward,
    #[error("paired verifier reward must be between zero and one")]
    InvalidReward,
    #[error("primary-only baseline violates Runtime safety")]
    UnsafeBaseline,
    #[error("paired baseline metric cannot be zero")]
    ZeroBaselineMetric,
    #[error("paired metric exceeds the supported range")]
    MetricOverflow,
    #[error("paired evidence exceeds the supported sample count")]
    TooManyPairs,
}

#[cfg(test)]
#[path = "../tests/unit/evaluation.rs"]
mod tests;
