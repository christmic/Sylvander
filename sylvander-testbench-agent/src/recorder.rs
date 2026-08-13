//! Lossless conversion from Sylvander's provider-neutral Agent events to ATIF.

use serde_json::{Map, json};
use sylvander_agent::event::AgentEvent;
use sylvander_llm_core::TokenUsage;
use thiserror::Error;

use crate::atif::{
    Agent, FinalMetrics, Metrics, Observation, ObservationResult, Source, Step, ToolCall,
    Trajectory,
};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RecorderError {
    #[error("Agent event order is invalid: {0}")]
    EventOrder(&'static str),
    #[error("tool call arguments must be a JSON object")]
    NonObjectToolArguments,
    #[error("Agent execution failed")]
    AgentFailed,
    #[error("trajectory is incomplete")]
    Incomplete,
    #[error("Harbor task isolation was not attested")]
    HarnessNotIsolated,
    #[error("generated trajectory violates ATIF: {0}")]
    InvalidAtif(&'static str),
}

#[derive(Debug)]
struct AgentStep {
    message: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
    results: Vec<ObservationResult>,
}

impl AgentStep {
    fn new() -> Self {
        Self {
            message: String::new(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            results: Vec::new(),
        }
    }
}

/// Records exactly one ATIF agent step for each Sylvander model iteration.
#[derive(Debug)]
pub struct TrajectoryRecorder {
    trajectory: Trajectory,
    current: Option<AgentStep>,
    completed: bool,
}

impl TrajectoryRecorder {
    #[must_use]
    pub fn new(
        session_id: impl Into<String>,
        model_name: impl Into<String>,
        system_messages: impl IntoIterator<Item = String>,
        user_message: impl Into<String>,
    ) -> Self {
        let model_name = model_name.into();
        let mut steps = Vec::new();
        for message in system_messages {
            push_plain_step(&mut steps, Source::System, message);
        }
        push_plain_step(&mut steps, Source::User, user_message.into());
        Self {
            trajectory: Trajectory {
                schema_version: "ATIF-v1.7".into(),
                session_id: Some(session_id.into()),
                trajectory_id: None,
                agent: Agent {
                    name: "sylvander".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    model_name: Some(model_name),
                    tool_definitions: None,
                },
                steps,
                notes: None,
                final_metrics: None,
                extra: None,
            },
            current: None,
            completed: false,
        }
    }

    pub fn record(&mut self, event: AgentEvent) -> Result<(), RecorderError> {
        match event {
            AgentEvent::IterationStart { .. } => {
                if self.current.replace(AgentStep::new()).is_some() {
                    return Err(RecorderError::EventOrder("iteration started twice"));
                }
            }
            AgentEvent::TextChunk(delta) => self.current_mut()?.message.push_str(&delta),
            AgentEvent::ThinkingChunk(delta) => self.current_mut()?.reasoning.push_str(&delta),
            AgentEvent::ToolCallStart { id, name, input } => {
                let arguments = input
                    .as_object()
                    .cloned()
                    .ok_or(RecorderError::NonObjectToolArguments)?;
                self.current_mut()?.tool_calls.push(ToolCall {
                    tool_call_id: id,
                    function_name: name,
                    arguments,
                });
            }
            AgentEvent::ToolCallEnd {
                id,
                output,
                is_error,
                ..
            } => self.push_result(id, output, is_error)?,
            AgentEvent::ToolRejected { id, reason, .. } => self.push_result(id, reason, true)?,
            AgentEvent::IterationEnd { provider_usage, .. } => {
                self.finish_iteration(provider_usage)?;
            }
            AgentEvent::Done(outcome) => {
                if self.current.is_some() {
                    return Err(RecorderError::EventOrder("done preceded iteration end"));
                }
                let usage = outcome.total_usage;
                self.trajectory.final_metrics = Some(FinalMetrics {
                    total_prompt_tokens: usage.total_input_tokens(),
                    total_completion_tokens: usage.output_tokens,
                    total_cached_tokens: usage.cache_read_tokens,
                    total_steps: u32::try_from(self.trajectory.steps.len()).unwrap_or(u32::MAX),
                });
                self.completed = true;
            }
            AgentEvent::Error(_) => return Err(RecorderError::AgentFailed),
            AgentEvent::ModelRetry { .. }
            | AgentEvent::ToolCallOutputDelta { .. }
            | AgentEvent::ToolTimedOut { .. }
            | AgentEvent::CompressionStarted
            | AgentEvent::Compressed { .. }
            | AgentEvent::HistoryCompacted { .. }
            | AgentEvent::AskUser { .. }
            | AgentEvent::UserAnswer { .. }
            | AgentEvent::PlanProposed { .. }
            | AgentEvent::PlanResolved { .. } => {}
        }
        Ok(())
    }

    pub fn finish(self) -> Result<Trajectory, RecorderError> {
        if !self.completed || self.current.is_some() {
            return Err(RecorderError::Incomplete);
        }
        self.trajectory
            .validate()
            .map_err(RecorderError::InvalidAtif)?;
        Ok(self.trajectory)
    }

    fn current_mut(&mut self) -> Result<&mut AgentStep, RecorderError> {
        self.current.as_mut().ok_or(RecorderError::EventOrder(
            "event occurred outside an iteration",
        ))
    }

    fn push_result(
        &mut self,
        id: String,
        content: String,
        is_error: bool,
    ) -> Result<(), RecorderError> {
        let mut extra = Map::new();
        extra.insert("is_error".into(), json!(is_error));
        self.current_mut()?.results.push(ObservationResult {
            source_call_id: Some(id),
            content: Some(content),
            extra: Some(extra),
        });
        Ok(())
    }

    fn finish_iteration(&mut self, usage: TokenUsage) -> Result<(), RecorderError> {
        let current = self.current.take().ok_or(RecorderError::EventOrder(
            "iteration ended before it started",
        ))?;
        let model_name = self.trajectory.agent.model_name.clone();
        self.trajectory.steps.push(Step {
            step_id: u32::try_from(self.trajectory.steps.len() + 1).unwrap_or(u32::MAX),
            source: Source::Agent,
            model_name,
            message: current.message,
            reasoning_content: (!current.reasoning.is_empty()).then_some(current.reasoning),
            tool_calls: (!current.tool_calls.is_empty()).then_some(current.tool_calls),
            observation: (!current.results.is_empty()).then_some(Observation {
                results: current.results,
            }),
            metrics: Some(Metrics {
                prompt_tokens: usage.total_input_tokens(),
                completion_tokens: usage.output_tokens,
                cached_tokens: usage.cache_read_tokens,
            }),
            llm_call_count: Some(1),
        });
        Ok(())
    }
}

fn push_plain_step(steps: &mut Vec<Step>, source: Source, message: String) {
    steps.push(Step {
        step_id: u32::try_from(steps.len() + 1).unwrap_or(u32::MAX),
        source,
        model_name: None,
        message,
        reasoning_content: None,
        tool_calls: None,
        observation: None,
        metrics: None,
        llm_call_count: None,
    });
}
