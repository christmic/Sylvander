//! Deterministic aggregation of normalized Agent benchmark evidence.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::matrix::AgentMatrixCoordinate;
use crate::result::{AgentBenchResult, AgentBenchStatus};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct AgentAggregateKey {
    pub benchmark_id: String,
    pub dataset_name: String,
    pub dataset_version: String,
    pub agent_revision: String,
    pub provider_id: String,
    pub protocol: String,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentAggregate {
    pub schema_version: u32,
    #[serde(flatten)]
    pub key: AgentAggregateKey,
    pub total_cells: u32,
    pub executed_cells: u32,
    pub passed_cells: u32,
    pub failed_cells: u32,
    pub infrastructure_errors: u32,
    pub not_run_cells: u32,
    pub not_applicable_cells: u32,
    pub mean_reward: Option<f64>,
    pub pass_rate: Option<f64>,
    pub mean_duration_ms: Option<f64>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AggregateError {
    #[error("normalized Agent result has an unsupported schema")]
    UnsupportedSchema,
    #[error("duplicate Agent benchmark coordinate")]
    DuplicateCoordinate,
    #[error("normalized Agent result has inconsistent status and reward")]
    InvalidResult,
    #[error("Agent aggregate counter overflow")]
    CounterOverflow,
}

pub fn aggregate_results(
    results: impl IntoIterator<Item = AgentBenchResult>,
) -> Result<Vec<AgentAggregate>, AggregateError> {
    let mut seen = BTreeSet::new();
    let mut groups = BTreeMap::<AgentAggregateKey, Accumulator>::new();
    for result in results {
        validate(&result)?;
        if !seen.insert(coordinate_key(&result.coordinate)) {
            return Err(AggregateError::DuplicateCoordinate);
        }
        groups
            .entry(group_key(&result.coordinate))
            .or_default()
            .add(&result)?;
    }
    Ok(groups
        .into_iter()
        .map(|(key, value)| value.finish(key))
        .collect())
}

#[derive(Debug, Default)]
struct Accumulator {
    total: u32,
    executed: u32,
    passed: u32,
    failed: u32,
    infrastructure: u32,
    not_run: u32,
    not_applicable: u32,
    reward_sum: f64,
    duration_sum: u64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: Option<u64>,
}

impl Accumulator {
    fn add(&mut self, result: &AgentBenchResult) -> Result<(), AggregateError> {
        self.total = increment(self.total)?;
        match result.status {
            AgentBenchStatus::Passed => {
                self.executed = increment(self.executed)?;
                self.passed = increment(self.passed)?;
            }
            AgentBenchStatus::Failed => {
                self.executed = increment(self.executed)?;
                self.failed = increment(self.failed)?;
            }
            AgentBenchStatus::InfrastructureError => {
                self.infrastructure = increment(self.infrastructure)?;
            }
            AgentBenchStatus::NotRun => self.not_run = increment(self.not_run)?,
            AgentBenchStatus::NotApplicable => {
                self.not_applicable = increment(self.not_applicable)?;
            }
        }
        if let Some(reward) = result.reward {
            self.reward_sum += reward;
        }
        self.duration_sum = add_u64(self.duration_sum, result.duration_ms)?;
        self.input_tokens = add_u64(self.input_tokens, result.input_tokens)?;
        self.output_tokens = add_u64(self.output_tokens, result.output_tokens)?;
        if let Some(tokens) = result.cached_tokens {
            self.cached_tokens = Some(add_u64(self.cached_tokens.unwrap_or(0), tokens)?);
        }
        Ok(())
    }

    fn finish(self, key: AgentAggregateKey) -> AgentAggregate {
        let executed = f64::from(self.executed);
        let total = f64::from(self.total);
        AgentAggregate {
            schema_version: 1,
            key,
            total_cells: self.total,
            executed_cells: self.executed,
            passed_cells: self.passed,
            failed_cells: self.failed,
            infrastructure_errors: self.infrastructure,
            not_run_cells: self.not_run,
            not_applicable_cells: self.not_applicable,
            mean_reward: (self.executed > 0).then_some(self.reward_sum / executed),
            pass_rate: (self.executed > 0).then_some(f64::from(self.passed) / executed),
            mean_duration_ms: (self.total > 0).then_some(self.duration_sum as f64 / total),
            total_input_tokens: self.input_tokens,
            total_output_tokens: self.output_tokens,
            total_cached_tokens: self.cached_tokens,
        }
    }
}

fn validate(result: &AgentBenchResult) -> Result<(), AggregateError> {
    if result.schema_version != 1 {
        return Err(AggregateError::UnsupportedSchema);
    }
    let executed = matches!(
        result.status,
        AgentBenchStatus::Passed | AgentBenchStatus::Failed
    );
    if executed != result.reward.is_some()
        || result
            .reward
            .is_some_and(|reward| !reward.is_finite() || !(0.0..=1.0).contains(&reward))
    {
        return Err(AggregateError::InvalidResult);
    }
    Ok(())
}

fn increment(value: u32) -> Result<u32, AggregateError> {
    value.checked_add(1).ok_or(AggregateError::CounterOverflow)
}

fn add_u64(left: u64, right: u64) -> Result<u64, AggregateError> {
    left.checked_add(right)
        .ok_or(AggregateError::CounterOverflow)
}

fn group_key(coordinate: &AgentMatrixCoordinate) -> AgentAggregateKey {
    AgentAggregateKey {
        benchmark_id: coordinate.benchmark_id.clone(),
        dataset_name: coordinate.dataset_name.clone(),
        dataset_version: coordinate.dataset_version.clone(),
        agent_revision: coordinate.agent_revision.clone(),
        provider_id: coordinate.provider_id.clone(),
        protocol: coordinate.protocol.clone(),
        model_id: coordinate.model_id.clone(),
    }
}

fn coordinate_key(coordinate: &AgentMatrixCoordinate) -> String {
    serde_json::to_string(coordinate).expect("Agent coordinate serialization is infallible")
}
