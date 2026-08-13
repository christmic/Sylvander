//! Lossless conversion from Sylvander's provider-neutral Agent events to ATIF.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use sylvander_agent::event::{AgentEvent, ModelRetryCause};
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
    #[error("Agent execution failed: {0}")]
    AgentFailed(String),
    #[error("trajectory is incomplete")]
    Incomplete,
    #[error("Harbor task isolation was not attested")]
    HarnessNotIsolated,
    #[error("generated trajectory violates ATIF: {0}")]
    InvalidAtif(&'static str),
    #[error("failed to encode trajectory checkpoint")]
    CheckpointEncoding,
    #[error("failed to persist trajectory checkpoint")]
    CheckpointIo,
}

#[derive(Debug, Clone)]
struct AgentStep {
    message: String,
    reasoning: String,
    tool_calls: Vec<ToolCall>,
    results: Vec<ObservationResult>,
}

/// Content-safe deployment identity retained with detailed trajectory evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAudit {
    provider_id: String,
    protocol: String,
    model_id: String,
    base_url: String,
    credential_fingerprint: String,
}

impl ProviderAudit {
    #[must_use]
    pub fn new(
        provider_id: impl Into<String>,
        protocol: impl Into<String>,
        model_id: impl Into<String>,
        base_url: impl Into<String>,
        credential: &str,
    ) -> Self {
        let digest = Sha256::digest(credential.as_bytes());
        let mut credential_fingerprint = String::with_capacity(16);
        for byte in &digest[..8] {
            write!(&mut credential_fingerprint, "{byte:02x}")
                .expect("writing to a String cannot fail");
        }
        Self {
            provider_id: provider_id.into(),
            protocol: protocol.into(),
            model_id: model_id.into(),
            base_url: base_url.into(),
            credential_fingerprint: format!("sha256:{credential_fingerprint}"),
        }
    }

    fn as_json(&self) -> Value {
        json!({
            "provider_id": self.provider_id,
            "protocol": self.protocol,
            "model_id": self.model_id,
            "base_url": self.base_url,
            "credential_fingerprint": self.credential_fingerprint,
        })
    }
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
    event_sequence: u64,
    events: Vec<Value>,
    provider_audit: Option<ProviderAudit>,
    terminal_error: Option<String>,
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
            event_sequence: 0,
            events: Vec::new(),
            provider_audit: None,
            terminal_error: None,
        }
    }

    #[must_use]
    pub fn with_provider_audit(mut self, audit: ProviderAudit) -> Self {
        self.provider_audit = Some(audit);
        self
    }

    pub fn record(&mut self, event: AgentEvent) -> Result<(), RecorderError> {
        self.record_observability(&event);
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
            AgentEvent::Error(error) => {
                self.terminal_error = Some(error.to_string());
                return Err(RecorderError::AgentFailed(error.to_string()));
            }
            AgentEvent::TurnTransition(_)
            | AgentEvent::ModelInvocationPrepared { .. }
            | AgentEvent::ModelResponsePrepared { .. }
            | AgentEvent::ToolCallPrepared { .. }
            | AgentEvent::ModelRetry { .. }
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

    /// Return a valid ATIF checkpoint, including the currently active partial
    /// iteration and a content-safe event ledger.
    #[must_use]
    pub fn snapshot(&self) -> Trajectory {
        let mut trajectory = self.trajectory.clone();
        if let Some(current) = &self.current {
            trajectory
                .steps
                .push(agent_step(&trajectory, current.clone(), None));
        }
        let status = if self.completed {
            "completed"
        } else if self.terminal_error.is_some() {
            "failed"
        } else {
            "running"
        };
        let mut observability = Map::new();
        observability.insert("status".into(), json!(status));
        observability.insert("events".into(), Value::Array(self.events.clone()));
        if let Some(audit) = &self.provider_audit {
            observability.insert("provider".into(), audit.as_json());
        }
        if let Some(error) = &self.terminal_error {
            observability.insert("terminal_error".into(), json!(error));
        }
        trajectory.extra = Some(Map::from_iter([(
            "sylvander_observability".into(),
            Value::Object(observability),
        )]));
        trajectory
    }

    /// Atomically replace one durable checkpoint. A process killed between
    /// writes leaves the preceding complete JSON document available to Harbor.
    pub async fn checkpoint(&self, path: &Path) -> Result<(), RecorderError> {
        persist_trajectory(path, &self.snapshot()).await
    }

    pub fn finish(self) -> Result<Trajectory, RecorderError> {
        if !self.completed || self.current.is_some() {
            return Err(RecorderError::Incomplete);
        }
        let mut trajectory = self.snapshot();
        trajectory.validate().map_err(RecorderError::InvalidAtif)?;
        trajectory.final_metrics = self.trajectory.final_metrics;
        Ok(trajectory)
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
        let step = agent_step(&self.trajectory, current, Some(usage));
        self.trajectory.steps.push(step);
        Ok(())
    }

    fn record_observability(&mut self, event: &AgentEvent) {
        self.event_sequence = self.event_sequence.saturating_add(1);
        let mut value = event_observation(event);
        let object = value
            .as_object_mut()
            .expect("event observation is always an object");
        object.insert("sequence".into(), json!(self.event_sequence));
        object.insert("recorded_at_unix_ms".into(), json!(unix_time_ms()));
        self.events.push(value);
    }
}

pub async fn persist_trajectory(path: &Path, trajectory: &Trajectory) -> Result<(), RecorderError> {
    let encoded =
        serde_json::to_vec_pretty(trajectory).map_err(|_| RecorderError::CheckpointEncoding)?;
    let parent = path.parent().ok_or(RecorderError::CheckpointIo)?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|_| RecorderError::CheckpointIo)?;
    let temporary = parent.join(format!(
        ".{}.checkpoint-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("trajectory.json"),
        std::process::id()
    ));
    tokio::fs::write(&temporary, encoded)
        .await
        .map_err(|_| RecorderError::CheckpointIo)?;
    tokio::fs::rename(&temporary, path)
        .await
        .map_err(|_| RecorderError::CheckpointIo)
}

fn agent_step(trajectory: &Trajectory, current: AgentStep, usage: Option<TokenUsage>) -> Step {
    Step {
        step_id: u32::try_from(trajectory.steps.len() + 1).unwrap_or(u32::MAX),
        source: Source::Agent,
        model_name: trajectory.agent.model_name.clone(),
        message: current.message,
        reasoning_content: (!current.reasoning.is_empty()).then_some(current.reasoning),
        tool_calls: (!current.tool_calls.is_empty()).then_some(current.tool_calls),
        observation: (!current.results.is_empty()).then_some(Observation {
            results: current.results,
        }),
        metrics: usage.map(|usage| Metrics {
            prompt_tokens: usage.total_input_tokens(),
            completion_tokens: usage.output_tokens,
            cached_tokens: usage.cache_read_tokens,
        }),
        llm_call_count: usage.map(|_| 1),
    }
}

fn event_observation(event: &AgentEvent) -> Value {
    match event {
        AgentEvent::TurnTransition(transition) => json!({
            "kind": "turn_transition",
            "turn_sequence": transition.sequence,
            "iteration": transition.iteration,
            "from": transition.from.as_str(),
            "to": transition.to.as_str(),
            "reason": transition.reason.as_str(),
            "continuation": transition
                .continuation
                .map(sylvander_agent::prelude::TurnContinuationReason::as_str),
        }),
        AgentEvent::IterationStart { iteration } => {
            json!({"kind": "iteration_started", "iteration": iteration})
        }
        AgentEvent::ModelInvocationPrepared {
            iteration,
            invocation_id,
            request_digest,
        } => json!({
            "kind": "model_invocation_prepared",
            "iteration": iteration,
            "invocation_id": invocation_id,
            "request_digest": request_digest,
        }),
        AgentEvent::TextChunk(delta) => {
            json!({"kind": "text_delta", "bytes": delta.len()})
        }
        AgentEvent::ThinkingChunk(delta) => {
            json!({"kind": "thinking_delta", "bytes": delta.len()})
        }
        AgentEvent::ModelRetry {
            attempt,
            max_attempts,
            delay_ms,
            reason,
            cause,
        } => json!({
            "kind": "model_retry",
            "attempt": attempt,
            "max_attempts": max_attempts,
            "delay_ms": delay_ms,
            "cause": retry_cause(*cause),
            "reason": reason,
        }),
        AgentEvent::ModelResponsePrepared {
            iteration,
            terminal,
            ..
        } => json!({
            "kind": "model_response_prepared",
            "iteration": iteration,
            "terminal": terminal,
        }),
        AgentEvent::ToolCallPrepared {
            id,
            invocation_id,
            name,
            invocation_class,
            recovery_policy,
            input_digest,
            capability_revision,
        } => json!({
            "kind": "tool_prepared",
            "call_id": id,
            "invocation_id": invocation_id,
            "tool_name": name,
            "invocation_class": invocation_class.map(|value| format!("{value:?}")),
            "recovery_policy": format!("{recovery_policy:?}"),
            "input_digest": input_digest,
            "capability_revision": capability_revision,
        }),
        AgentEvent::ToolCallStart { id, name, input } => json!({
            "kind": "tool_started",
            "call_id": id,
            "tool_name": name,
            "input_bytes": input.to_string().len(),
        }),
        AgentEvent::ToolCallOutputDelta { id, name, delta } => json!({
            "kind": "tool_output_delta",
            "call_id": id,
            "tool_name": name,
            "bytes": delta.len(),
        }),
        AgentEvent::ToolTimedOut {
            id,
            name,
            timeout_secs,
        } => json!({
            "kind": "tool_timed_out",
            "call_id": id,
            "tool_name": name,
            "timeout_secs": timeout_secs,
        }),
        AgentEvent::ToolCallEnd {
            id,
            name,
            output,
            is_error,
            failure_kind,
        } => json!({
            "kind": "tool_finished",
            "call_id": id,
            "tool_name": name,
            "succeeded": !is_error,
            "failure_kind": failure_kind.map(|value| format!("{value:?}")),
            "output_bytes": output.len(),
        }),
        AgentEvent::ToolRejected { id, name, .. } => json!({
            "kind": "tool_rejected",
            "call_id": id,
            "tool_name": name,
        }),
        AgentEvent::IterationEnd {
            iteration,
            response_id,
            provider_usage,
            ..
        } => json!({
            "kind": "iteration_finished",
            "iteration": iteration,
            "response_id": response_id,
            "prompt_tokens": provider_usage.total_input_tokens(),
            "completion_tokens": provider_usage.output_tokens,
            "cache_read_tokens": provider_usage.cache_read_tokens,
            "cache_write_tokens": provider_usage.cache_write_tokens,
        }),
        AgentEvent::Done(outcome) => json!({
            "kind": "completed",
            "iterations": outcome.iterations,
            "response_id": outcome.final_response.id,
        }),
        AgentEvent::Error(error) => json!({"kind": "failed", "reason": error.to_string()}),
        AgentEvent::CompressionStarted => json!({"kind": "compression_started"}),
        AgentEvent::Compressed { layers } => {
            json!({"kind": "compressed", "layers": layers.len()})
        }
        AgentEvent::HistoryCompacted { layers, .. } => {
            json!({"kind": "history_compacted", "layers": layers.len()})
        }
        AgentEvent::AskUser { call_id, .. } => {
            json!({"kind": "user_input_requested", "call_id": call_id})
        }
        AgentEvent::UserAnswer { call_id, .. } => {
            json!({"kind": "user_input_received", "call_id": call_id})
        }
        AgentEvent::PlanProposed { plan_id, steps } => {
            json!({"kind": "plan_proposed", "plan_id": plan_id, "steps": steps.len()})
        }
        AgentEvent::PlanResolved { plan_id, .. } => {
            json!({"kind": "plan_resolved", "plan_id": plan_id})
        }
    }
}

const fn retry_cause(cause: ModelRetryCause) -> &'static str {
    match cause {
        ModelRetryCause::RateLimit => "rate_limit",
        ModelRetryCause::Server => "server",
        ModelRetryCause::Network => "network",
        ModelRetryCause::Stream => "stream",
        ModelRetryCause::Other => "other",
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
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
