//! Fail-closed ingestion of Harbor's per-trial `result.json`.
//!
//! The narrow wire view follows Harbor `TrialResult`, `VerifierResult`, and
//! `AgentContext` at revision `ea2fee78517f2e591bad69fcf1e6731f9c23ec99`.

use std::collections::BTreeMap;

use chrono::DateTime;
use serde::Deserialize;

use crate::atif::{Source, Trajectory};
use crate::matrix::AgentMatrixCoordinate;
use crate::result::{AgentBenchResult, AgentBenchStatus, RepositoryState};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HarborTrialResult {
    pub task_name: String,
    pub agent_info: HarborAgentInfo,
    pub agent_result: Option<HarborAgentContext>,
    pub verifier_result: Option<HarborVerifierResult>,
    pub exception_info: Option<HarborExceptionInfo>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HarborAgentInfo {
    pub name: String,
    pub model_info: Option<HarborModelInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HarborModelInfo {
    pub name: String,
    pub provider: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct HarborAgentContext {
    pub n_input_tokens: Option<u64>,
    pub n_cache_tokens: Option<u64>,
    pub n_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HarborVerifierResult {
    pub rewards: Option<BTreeMap<String, f64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct HarborExceptionInfo {
    pub exception_type: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HarborResultError {
    #[error("Harbor result coordinate does not match the planned cell")]
    CoordinateMismatch,
    #[error("Harbor result contains ambiguous verifier rewards")]
    AmbiguousReward,
    #[error("Harbor result timing is missing or invalid")]
    InvalidTiming,
    #[error("Harbor token totals disagree with the ATIF trajectory")]
    MetricsMismatch,
    #[error("ATIF trajectory is invalid or incomplete")]
    InvalidTrajectory,
    #[error("normalized result violates its evidence contract")]
    InvalidNormalizedResult,
}

pub fn normalize_harbor_result(
    coordinate: AgentMatrixCoordinate,
    repository: RepositoryState,
    harness_revision: impl Into<String>,
    trial: &HarborTrialResult,
    trajectory: &Trajectory,
) -> Result<AgentBenchResult, HarborResultError> {
    validate_coordinate(&coordinate, trial, trajectory)?;
    trajectory
        .validate()
        .map_err(|_| HarborResultError::InvalidTrajectory)?;
    let duration_ms = duration_ms(trial)?;
    let reward = primary_reward(trial)?;
    let trajectory_failed = trajectory
        .extra
        .as_ref()
        .and_then(|extra| extra.get("sylvander_observability"))
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("failed");
    let (status, failure_kind) = match reward {
        Some(value) if value >= 1.0 => (AgentBenchStatus::Passed, None),
        Some(_) => (AgentBenchStatus::Failed, None),
        None if trajectory_failed => (
            AgentBenchStatus::AgentError,
            Some("agent_execution_error".to_owned()),
        ),
        None => (
            AgentBenchStatus::InfrastructureError,
            Some(if trial.exception_info.is_some() {
                "harbor_exception".to_owned()
            } else {
                "missing_verifier_result".to_owned()
            }),
        ),
    };
    let metrics = trajectory_metrics(trajectory)?;
    if let Some(context) = trial.agent_result
        && (context.n_input_tokens.is_some()
            || context.n_output_tokens.is_some()
            || context.n_cache_tokens.is_some())
        && (context.n_input_tokens != Some(metrics.0)
            || context.n_output_tokens != Some(metrics.1)
            || context.n_cache_tokens != metrics.2)
    {
        return Err(HarborResultError::MetricsMismatch);
    }
    let iterations = trajectory
        .steps
        .iter()
        .filter(|step| step.source == Source::Agent)
        .count()
        .try_into()
        .map_err(|_| HarborResultError::InvalidTrajectory)?;
    let tool_calls = trajectory
        .steps
        .iter()
        .filter_map(|step| step.tool_calls.as_ref())
        .map(Vec::len)
        .sum::<usize>()
        .try_into()
        .map_err(|_| HarborResultError::InvalidTrajectory)?;
    AgentBenchResult::recorded(
        coordinate,
        status,
        reward,
        repository,
        harness_revision,
        duration_ms,
        iterations,
        tool_calls,
        metrics.0,
        metrics.1,
        metrics.2,
        failure_kind,
    )
    .map_err(|_| HarborResultError::InvalidNormalizedResult)
}

fn trajectory_metrics(
    trajectory: &Trajectory,
) -> Result<(u64, u64, Option<u64>), HarborResultError> {
    if let Some(metrics) = trajectory.final_metrics {
        return Ok((
            metrics.total_prompt_tokens,
            metrics.total_completion_tokens,
            metrics.total_cached_tokens,
        ));
    }
    let mut prompt = 0_u64;
    let mut completion = 0_u64;
    let mut cached = None;
    for metrics in trajectory.steps.iter().filter_map(|step| step.metrics) {
        prompt = prompt
            .checked_add(metrics.prompt_tokens)
            .ok_or(HarborResultError::InvalidTrajectory)?;
        completion = completion
            .checked_add(metrics.completion_tokens)
            .ok_or(HarborResultError::InvalidTrajectory)?;
        if let Some(tokens) = metrics.cached_tokens {
            cached = Some(
                cached
                    .unwrap_or(0_u64)
                    .checked_add(tokens)
                    .ok_or(HarborResultError::InvalidTrajectory)?,
            );
        }
    }
    Ok((prompt, completion, cached))
}

fn validate_coordinate(
    coordinate: &AgentMatrixCoordinate,
    trial: &HarborTrialResult,
    trajectory: &Trajectory,
) -> Result<(), HarborResultError> {
    let model = trial
        .agent_info
        .model_info
        .as_ref()
        .ok_or(HarborResultError::CoordinateMismatch)?;
    if trial.task_name != coordinate.task_id
        || trial.agent_info.name != "sylvander"
        || model.name != coordinate.model_id
        || model.provider.as_deref() != Some(coordinate.provider_id.as_str())
        || trajectory.agent.model_name.as_deref()
            != Some(format!("{}/{}", coordinate.provider_id, coordinate.model_id).as_str())
    {
        return Err(HarborResultError::CoordinateMismatch);
    }
    Ok(())
}

fn primary_reward(trial: &HarborTrialResult) -> Result<Option<f64>, HarborResultError> {
    let Some(rewards) = trial
        .verifier_result
        .as_ref()
        .and_then(|result| result.rewards.as_ref())
    else {
        return Ok(None);
    };
    if let Some(reward) = rewards.get("reward") {
        return Ok(Some(*reward));
    }
    if rewards.len() == 1 {
        return Ok(rewards.values().next().copied());
    }
    Err(HarborResultError::AmbiguousReward)
}

fn duration_ms(trial: &HarborTrialResult) -> Result<u64, HarborResultError> {
    let started = DateTime::parse_from_rfc3339(
        trial
            .started_at
            .as_deref()
            .ok_or(HarborResultError::InvalidTiming)?,
    )
    .map_err(|_| HarborResultError::InvalidTiming)?;
    let finished = DateTime::parse_from_rfc3339(
        trial
            .finished_at
            .as_deref()
            .ok_or(HarborResultError::InvalidTiming)?,
    )
    .map_err(|_| HarborResultError::InvalidTiming)?;
    u64::try_from((finished - started).num_milliseconds())
        .map_err(|_| HarborResultError::InvalidTiming)
}
