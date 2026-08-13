//! Harbor Agent Trajectory Interchange Format (ATIF) v1.7 values.
//!
//! The field contract follows Harbor commit
//! `ea2fee78517f2e591bad69fcf1e6731f9c23ec99`, specifically
//! `src/harbor/models/trajectories/{trajectory,step,agent,tool_call,
//! observation,observation_result,metrics,final_metrics}.py`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trajectory {
    pub schema_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    pub agent: Agent,
    pub steps: Vec<Step>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<FinalMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

impl Trajectory {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != "ATIF-v1.7" {
            return Err("only ATIF-v1.7 output is supported");
        }
        if self.steps.is_empty() {
            return Err("trajectory must contain at least one step");
        }
        if self
            .steps
            .iter()
            .enumerate()
            .any(|(index, step)| step.step_id != index as u32 + 1)
        {
            return Err("trajectory step identifiers must be sequential from one");
        }
        for step in &self.steps {
            if step.source != Source::Agent
                && (step.model_name.is_some()
                    || step.reasoning_content.is_some()
                    || step.tool_calls.is_some()
                    || step.metrics.is_some())
            {
                return Err("model fields are valid only on agent steps");
            }
            if let Some(observation) = &step.observation {
                for result in &observation.results {
                    if result.source_call_id.as_ref().is_some_and(|id| {
                        !step
                            .tool_calls
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .any(|call| &call.tool_call_id == id)
                    }) {
                        return Err("observation references an unknown tool call");
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    System,
    User,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub step_id: u32,
    pub source: Source,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub results: Vec<ObservationResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metrics {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinalMetrics {
    pub total_prompt_tokens: u64,
    pub total_completion_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cached_tokens: Option<u64>,
    pub total_steps: u32,
}
