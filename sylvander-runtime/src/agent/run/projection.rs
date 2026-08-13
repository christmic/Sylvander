//! Type-safe projections from Agent and persistence facts to Runtime APIs.

use sylvander_agent::plan_gate::PlanDecision;
use sylvander_agent::tool::{ToolSourceFeature, ToolSourceKind, ToolSourceStatus};
use sylvander_agent::turn::event::ModelRetryCause;
use sylvander_api::{
    PlatformAuthStatus, PlatformFeature, PlatformFeatureKind, PlatformFeatureStatus, PlatformTrust,
};
use sylvander_llm_core::{ModelCapabilities, TokenUsage};

use crate::observability::{RuntimeFailureKind, RuntimePersistenceOperation};
use crate::storage::session::TurnFailureKind;

use super::{AgentRunError, SessionPersistenceOperation};

pub(super) fn agent_plan_decision(decision: &sylvander_api::PlanDecision) -> PlanDecision {
    match decision {
        sylvander_api::PlanDecision::Approved => PlanDecision::Approved,
        sylvander_api::PlanDecision::Revised { steps } => PlanDecision::Revised {
            steps: steps.clone(),
        },
        sylvander_api::PlanDecision::Rejected { reason } => PlanDecision::Rejected {
            reason: reason.clone(),
        },
    }
}

pub(super) fn public_retry_cause(cause: ModelRetryCause) -> sylvander_api::RetryCause {
    match cause {
        ModelRetryCause::RateLimit => sylvander_api::RetryCause::RateLimit,
        ModelRetryCause::Server => sylvander_api::RetryCause::Server,
        ModelRetryCause::Network => sylvander_api::RetryCause::Network,
        ModelRetryCause::Stream => sylvander_api::RetryCause::Stream,
        ModelRetryCause::Other => sylvander_api::RetryCause::Other,
    }
}

pub(super) fn public_tool_feature(feature: ToolSourceFeature) -> PlatformFeature {
    PlatformFeature {
        kind: match feature.kind {
            ToolSourceKind::Mcp => PlatformFeatureKind::Mcp,
            ToolSourceKind::Hook => PlatformFeatureKind::Hook,
            ToolSourceKind::Extension => PlatformFeatureKind::Extension,
        },
        name: feature.name,
        status: match feature.status {
            ToolSourceStatus::Active => PlatformFeatureStatus::Active,
            ToolSourceStatus::Configured => PlatformFeatureStatus::Configured,
            ToolSourceStatus::Degraded => PlatformFeatureStatus::Degraded,
            ToolSourceStatus::Unavailable => PlatformFeatureStatus::Unavailable,
        },
        summary: feature.summary,
        source: feature.source,
        trust: Some(match feature.kind {
            ToolSourceKind::Hook => PlatformTrust::User,
            ToolSourceKind::Mcp | ToolSourceKind::Extension => PlatformTrust::External,
        }),
        auth: if feature.requires_authentication {
            PlatformAuthStatus::Configured
        } else {
            PlatformAuthStatus::NotRequired
        },
        capabilities: feature.capabilities,
        reloadable: feature.reloadable,
    }
}

pub(super) fn public_capability_names(
    capabilities: ModelCapabilities,
) -> Vec<sylvander_api::ModelCapability> {
    [
        (
            ModelCapabilities::REASONING,
            sylvander_api::ModelCapability::ExtendedThinking,
        ),
        (
            ModelCapabilities::PROMPT_CACHING,
            sylvander_api::ModelCapability::PromptCaching,
        ),
        (
            ModelCapabilities::STRUCTURED_OUTPUT,
            sylvander_api::ModelCapability::StructuredOutput,
        ),
        (
            ModelCapabilities::TOOL_USE,
            sylvander_api::ModelCapability::ToolUse,
        ),
        (
            ModelCapabilities::VISION,
            sylvander_api::ModelCapability::Vision,
        ),
        (
            ModelCapabilities::DOCUMENT_INPUT,
            sylvander_api::ModelCapability::DocumentInput,
        ),
    ]
    .into_iter()
    .filter_map(|(flag, name)| capabilities.contains(flag).then_some(name))
    .collect()
}

pub(super) fn usage_cost_nano_usd(
    pricing: sylvander_api::ModelPricing,
    usage: &TokenUsage,
) -> Option<u64> {
    fn component(tokens: u64, rate: u64) -> u128 {
        // rate is micro-USD / 1M tokens; nano-USD therefore divides by 1,000.
        (u128::from(tokens) * u128::from(rate) + 500) / 1_000
    }

    let cache_write = usage.cache_write_tokens.unwrap_or(0);
    let cache_read = usage.cache_read_tokens.unwrap_or(0);
    let mut total = component(usage.input_tokens, pricing.input_usd_micros_per_million)
        + component(usage.output_tokens, pricing.output_usd_micros_per_million);
    if cache_write > 0 {
        total += component(cache_write, pricing.cache_write_usd_micros_per_million?);
    }
    if cache_read > 0 {
        total += component(cache_read, pricing.cache_read_usd_micros_per_million?);
    }
    total.try_into().ok()
}

pub(super) fn public_compaction_report(
    automatic: bool,
    layers: &[sylvander_agent::compress::layer::LayerReport],
) -> sylvander_api::CompactionReport {
    sylvander_api::CompactionReport {
        automatic,
        removed_messages: sylvander_agent::compress::layer::total_removed(layers),
        condensed_blocks: sylvander_agent::compress::layer::total_condensed(layers),
        freed_tokens: sylvander_agent::compress::layer::total_freed(layers),
        summary: compaction_summary(layers),
    }
}

pub(super) fn compaction_summary(
    layers: &[sylvander_agent::compress::layer::LayerReport],
) -> Option<String> {
    layers.iter().find_map(|layer| {
        layer
            .details
            .as_ref()?
            .get("summary")?
            .as_str()
            .map(str::to_owned)
    })
}

pub(super) fn runtime_failure_kind(error: &AgentRunError) -> RuntimeFailureKind {
    match error {
        AgentRunError::UnknownSession(_) => RuntimeFailureKind::UnknownSession,
        AgentRunError::Authentication(_) => RuntimeFailureKind::Authentication,
        AgentRunError::Loop(_) => RuntimeFailureKind::AgentLoop,
        AgentRunError::Build(_) | AgentRunError::Configuration(_) => {
            RuntimeFailureKind::Configuration
        }
        AgentRunError::SessionPersistence { .. } => RuntimeFailureKind::Persistence,
    }
}

pub(super) fn turn_failure_kind(error: &AgentRunError) -> TurnFailureKind {
    match error {
        AgentRunError::UnknownSession(_) => TurnFailureKind::UnknownSession,
        AgentRunError::Authentication(_) => TurnFailureKind::Authentication,
        AgentRunError::Loop(_) => TurnFailureKind::AgentLoop,
        AgentRunError::Build(_) | AgentRunError::Configuration(_) => TurnFailureKind::Configuration,
        AgentRunError::SessionPersistence { .. } => TurnFailureKind::Persistence,
    }
}

pub(super) fn runtime_persistence_operation(
    operation: SessionPersistenceOperation,
) -> RuntimePersistenceOperation {
    match operation {
        SessionPersistenceOperation::InspectSession => RuntimePersistenceOperation::InspectSession,
        SessionPersistenceOperation::CreateSession => RuntimePersistenceOperation::CreateSession,
        SessionPersistenceOperation::RestoreHistory => RuntimePersistenceOperation::RestoreHistory,
        SessionPersistenceOperation::BeginTurn => RuntimePersistenceOperation::BeginTurn,
        SessionPersistenceOperation::BeginToolCall => RuntimePersistenceOperation::BeginToolCall,
        SessionPersistenceOperation::FinishToolCall => RuntimePersistenceOperation::FinishToolCall,
        SessionPersistenceOperation::RecordUsage => RuntimePersistenceOperation::RecordUsage,
        SessionPersistenceOperation::CompleteTurn => RuntimePersistenceOperation::CompleteTurn,
        SessionPersistenceOperation::FinishTurn => RuntimePersistenceOperation::FinishTurn,
        SessionPersistenceOperation::ReplaceHistory => RuntimePersistenceOperation::ReplaceHistory,
    }
}
