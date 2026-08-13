//! Declarative external Agent benchmark matrix.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCapability {
    Terminal,
    Filesystem,
    Git,
    InteractiveUser,
    DomainTools,
    Browser,
    Multimodal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBinding {
    pub task_id: String,
    pub required_capabilities: BTreeSet<AgentCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkBinding {
    pub benchmark_id: String,
    pub dataset_name: String,
    pub dataset_version: String,
    pub tasks: Vec<TaskBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentBinding {
    pub agent_revision: String,
    pub provider_id: String,
    pub protocol: String,
    pub model_id: String,
    pub capabilities: BTreeSet<AgentCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentBenchMatrix {
    pub schema_version: u32,
    pub repetitions: u32,
    pub benchmarks: Vec<BenchmarkBinding>,
    pub deployments: Vec<DeploymentBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applicability {
    Required,
    NotApplicableCapability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMatrixCoordinate {
    pub benchmark_id: String,
    pub dataset_name: String,
    pub dataset_version: String,
    pub task_id: String,
    pub agent_revision: String,
    pub provider_id: String,
    pub protocol: String,
    pub model_id: String,
    pub run_ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMatrixCell {
    pub coordinate: AgentMatrixCoordinate,
    pub applicability: Applicability,
}

impl AgentBenchMatrix {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("unsupported Agent matrix schema version");
        }
        if !(1..=10).contains(&self.repetitions) {
            return Err("Agent matrix repetitions must be between 1 and 10");
        }
        if self.benchmarks.is_empty() || self.deployments.is_empty() {
            return Err("Agent matrix dimensions must not be empty");
        }
        let mut task_keys = BTreeSet::new();
        for benchmark in &self.benchmarks {
            if benchmark.benchmark_id.is_empty()
                || benchmark.dataset_name.is_empty()
                || benchmark.dataset_version.is_empty()
                || benchmark.tasks.is_empty()
            {
                return Err("benchmark binding must be versioned and non-empty");
            }
            for task in &benchmark.tasks {
                if task.task_id.is_empty()
                    || !task_keys.insert((
                        benchmark.benchmark_id.as_str(),
                        benchmark.dataset_name.as_str(),
                        benchmark.dataset_version.as_str(),
                        task.task_id.as_str(),
                    ))
                {
                    return Err("benchmark task coordinates must be non-empty and unique");
                }
            }
        }
        let mut deployments = BTreeSet::new();
        for deployment in &self.deployments {
            let key = (
                deployment.agent_revision.as_str(),
                deployment.provider_id.as_str(),
                deployment.protocol.as_str(),
                deployment.model_id.as_str(),
            );
            if key.0.is_empty()
                || key.1.is_empty()
                || key.2.is_empty()
                || key.3.is_empty()
                || !deployments.insert(key)
            {
                return Err("Agent deployment coordinates must be non-empty and unique");
            }
        }
        Ok(())
    }

    pub fn expand(&self) -> Result<Vec<AgentMatrixCell>, &'static str> {
        self.validate()?;
        let mut cells = Vec::new();
        for benchmark in &self.benchmarks {
            for task in &benchmark.tasks {
                for deployment in &self.deployments {
                    let applicability = if task
                        .required_capabilities
                        .is_subset(&deployment.capabilities)
                    {
                        Applicability::Required
                    } else {
                        Applicability::NotApplicableCapability
                    };
                    for run_ordinal in 1..=self.repetitions {
                        cells.push(AgentMatrixCell {
                            coordinate: AgentMatrixCoordinate {
                                benchmark_id: benchmark.benchmark_id.clone(),
                                dataset_name: benchmark.dataset_name.clone(),
                                dataset_version: benchmark.dataset_version.clone(),
                                task_id: task.task_id.clone(),
                                agent_revision: deployment.agent_revision.clone(),
                                provider_id: deployment.provider_id.clone(),
                                protocol: deployment.protocol.clone(),
                                model_id: deployment.model_id.clone(),
                                run_ordinal,
                            },
                            applicability,
                        });
                    }
                }
            }
        }
        Ok(cells)
    }
}
