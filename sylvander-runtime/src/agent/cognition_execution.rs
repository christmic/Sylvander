//! Durable execution sandwich for one bounded text cognition consultation.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sylvander_api::AgentInstanceId;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ModelInfo, ModelProvider, ModelRef, ModelRequest, ModelResponse,
    ModelStreamEvent, StopReason, SystemInstruction, TokenUsage,
    validate_model_request_capabilities,
};

use super::cognition::CognitiveRole;
use super::cognition_artifact::{CognitionArtifactKind, CognitionArtifactStore};
use crate::agent_definition::SessionId;
use crate::storage::session::{
    CognitionAdvance, CognitionExecutionPosition, CognitionFailureKind,
    CognitionFailurePersistence, CognitionInvocationId, CognitionInvocationSnapshot,
    CognitionInvocationStart, CognitionOutputPersistence, CognitionPromptPersistence,
    CognitionReceiptPersistence, CognitionRecoveryPolicy, SessionStore,
};

const COGNITION_TIMEOUT: Duration = Duration::from_mins(2);
const MAX_COGNITION_OUTPUT_TOKENS: u32 = 4_096;
const MAX_COGNITION_PROMPT_BYTES: usize = 32_768;

#[derive(Clone)]
pub struct CognitionExecutionRequest {
    pub session_id: SessionId,
    pub turn_id: String,
    pub agent_instance_id: AgentInstanceId,
    pub invocation_id: CognitionInvocationId,
    pub role: CognitiveRole,
    pub model: ModelInfo,
    pub prompt: String,
    pub max_turn_calls: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CognitionExecutionResult {
    pub invocation_id: CognitionInvocationId,
    pub provider_response_id: String,
    pub text: String,
    pub artifact_locator: String,
    pub output_digest: String,
    pub usage: TokenUsage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CognitionExecutionError {
    #[error("cognition request is invalid")]
    InvalidRequest,
    #[error("cognition model is incompatible")]
    IncompatibleModel,
    #[error("cognition provider failed")]
    Provider,
    #[error("cognition provider timed out")]
    TimedOut,
    #[error("cognition provider response is invalid")]
    InvalidResponse,
    #[error("cognition durable state is unavailable")]
    Persistence,
    #[error("cognition encrypted artifact is unavailable")]
    Artifact,
    #[error("cognition provider receipt does not exist")]
    ReceiptMissing,
}

/// Run exactly one fresh call. Once inference starts, this function never
/// retries the provider; restart recovery is receipt-only.
pub async fn execute_cognition(
    store: Arc<dyn SessionStore>,
    artifacts: Arc<dyn CognitionArtifactStore>,
    provider: Arc<dyn ModelProvider>,
    request: CognitionExecutionRequest,
) -> Result<CognitionExecutionResult, CognitionExecutionError> {
    validate_request(&request)?;
    store
        .begin_cognition(CognitionInvocationStart {
            session_id: request.session_id.clone(),
            turn_id: request.turn_id.clone(),
            agent_instance_id: request.agent_instance_id.clone(),
            invocation_id: request.invocation_id.clone(),
            role: request.role,
            provider_id: request.model.reference.provider.clone(),
            model_id: request.model.reference.model.clone(),
            recovery_policy: CognitionRecoveryPolicy::RecoverFromReceipt,
            capability_revision: digest_capabilities(&request.model),
            input_digest: digest_prompt(&request.prompt),
            input_bytes: request.prompt.len() as u64,
            max_turn_calls: request.max_turn_calls,
        })
        .await
        .map_err(|_| CognitionExecutionError::Persistence)?;
    let prompt = artifacts
        .persist_exact(
            request.invocation_id.as_str(),
            CognitionArtifactKind::SourcePrompt,
            "text/plain; charset=utf-8",
            request.prompt.as_bytes().to_vec(),
        )
        .await
        .map_err(|_| CognitionExecutionError::Artifact)?;
    let revision = store
        .persist_cognition_prompt(CognitionPromptPersistence {
            invocation_id: request.invocation_id.clone(),
            expected_revision: 0,
            artifact_locator: prompt.locator,
        })
        .await
        .map_err(|_| CognitionExecutionError::Persistence)?;
    let revision = store
        .advance_cognition(CognitionAdvance {
            invocation_id: request.invocation_id.clone(),
            expected_revision: revision,
            expected_position: CognitionExecutionPosition::PromptPersisted,
            next_position: CognitionExecutionPosition::InferenceStarted,
        })
        .await
        .map_err(|_| CognitionExecutionError::Persistence)?;
    let response = match call_specialist(provider.as_ref(), &request).await {
        Ok(response) => response,
        Err(error) => {
            store
                .fail_cognition(CognitionFailurePersistence {
                    invocation_id: request.invocation_id,
                    expected_revision: revision,
                    failure_kind: failure_kind(error),
                })
                .await
                .map_err(|_| CognitionExecutionError::Persistence)?;
            return Err(error);
        }
    };
    finish_from_response(store, artifacts, request.invocation_id, revision, response).await
}

/// Complete a post-inference crash window from durable artifacts. This path
/// never invokes the provider and fails closed when its receipt is absent.
pub async fn recover_cognition_receipt(
    store: Arc<dyn SessionStore>,
    artifacts: Arc<dyn CognitionArtifactStore>,
    snapshot: CognitionInvocationSnapshot,
) -> Result<CognitionExecutionResult, CognitionExecutionError> {
    if snapshot.recovery_policy != CognitionRecoveryPolicy::RecoverFromReceipt
        || !matches!(
            snapshot.position,
            CognitionExecutionPosition::InferenceStarted
                | CognitionExecutionPosition::InferenceCompleted
                | CognitionExecutionPosition::ArtifactPersisted
                | CognitionExecutionPosition::ResultPersisted
        )
    {
        return Err(CognitionExecutionError::InvalidRequest);
    }
    let receipt = artifacts
        .load_exact(
            snapshot.invocation_id.as_str(),
            CognitionArtifactKind::ProviderReceipt,
        )
        .await
        .map_err(|_| CognitionExecutionError::Artifact)?
        .ok_or(CognitionExecutionError::ReceiptMissing)?;
    let response: ModelResponse = serde_json::from_slice(&receipt.payload)
        .map_err(|_| CognitionExecutionError::InvalidResponse)?;
    if response.model.provider != snapshot.provider_id || response.model.model != snapshot.model_id
    {
        return Err(CognitionExecutionError::InvalidResponse);
    }
    match snapshot.position {
        CognitionExecutionPosition::InferenceStarted => {
            finish_from_receipt(
                store,
                artifacts,
                snapshot.invocation_id,
                snapshot.ledger_revision,
                response,
                receipt.locator,
            )
            .await
        }
        CognitionExecutionPosition::InferenceCompleted => {
            persist_output(
                store,
                artifacts,
                snapshot.invocation_id,
                snapshot.ledger_revision,
                response,
            )
            .await
        }
        CognitionExecutionPosition::ArtifactPersisted => {
            let result = load_result(&artifacts, &snapshot, response).await?;
            store
                .complete_cognition(&snapshot.invocation_id, snapshot.ledger_revision)
                .await
                .map_err(|_| CognitionExecutionError::Persistence)?;
            Ok(result)
        }
        CognitionExecutionPosition::ResultPersisted => {
            load_result(&artifacts, &snapshot, response).await
        }
        _ => Err(CognitionExecutionError::InvalidRequest),
    }
}

async fn call_specialist(
    provider: &dyn ModelProvider,
    request: &CognitionExecutionRequest,
) -> Result<ModelResponse, CognitionExecutionError> {
    let model_request = ModelRequest {
        request_id: request.invocation_id.as_str().to_owned(),
        model: request.model.reference.clone(),
        system: vec![SystemInstruction {
            text: role_instruction(request.role).to_owned(),
            cache_hint: None,
        }],
        messages: vec![ChatMessage::user(request.prompt.clone())],
        tools: Vec::new(),
        max_output_tokens: request
            .model
            .max_output_tokens
            .min(MAX_COGNITION_OUTPUT_TOKENS),
        reasoning: None,
        output_schema: None,
    };
    validate_model_request_capabilities(&model_request, request.model.capabilities)
        .map_err(|_| CognitionExecutionError::IncompatibleModel)?;
    let stream = tokio::time::timeout(COGNITION_TIMEOUT, provider.complete_stream(model_request))
        .await
        .map_err(|_| CognitionExecutionError::TimedOut)?
        .map_err(|_| CognitionExecutionError::Provider)?;
    consume_response(stream, &request.model.reference).await
}

async fn consume_response(
    mut stream: sylvander_llm_core::ModelEventStream,
    expected: &ModelRef,
) -> Result<ModelResponse, CognitionExecutionError> {
    let mut completed = None;
    while let Some(event) = tokio::time::timeout(COGNITION_TIMEOUT, stream.next())
        .await
        .map_err(|_| CognitionExecutionError::TimedOut)?
    {
        let event = event.map_err(|_| CognitionExecutionError::Provider)?;
        if completed.is_some() {
            return Err(CognitionExecutionError::InvalidResponse);
        }
        if let ModelStreamEvent::Completed(response) = event {
            completed = Some(*response);
        }
    }
    let response = completed.ok_or(CognitionExecutionError::InvalidResponse)?;
    if response.model != *expected
        || response.text().trim().is_empty()
        || matches!(
            response.stop_reason,
            StopReason::ToolUse | StopReason::Paused
        )
        || response.content.iter().any(|block| {
            !matches!(
                block,
                ContentBlock::Text { .. } | ContentBlock::Reasoning { .. }
            )
        })
    {
        return Err(CognitionExecutionError::InvalidResponse);
    }
    Ok(response)
}

async fn finish_from_response(
    store: Arc<dyn SessionStore>,
    artifacts: Arc<dyn CognitionArtifactStore>,
    invocation_id: CognitionInvocationId,
    revision: u64,
    response: ModelResponse,
) -> Result<CognitionExecutionResult, CognitionExecutionError> {
    let payload =
        serde_json::to_vec(&response).map_err(|_| CognitionExecutionError::InvalidResponse)?;
    let receipt = artifacts
        .persist_exact(
            invocation_id.as_str(),
            CognitionArtifactKind::ProviderReceipt,
            "application/json",
            payload,
        )
        .await
        .map_err(|_| CognitionExecutionError::Artifact)?;
    finish_from_receipt(
        store,
        artifacts,
        invocation_id,
        revision,
        response,
        receipt.locator,
    )
    .await
}

async fn finish_from_receipt(
    store: Arc<dyn SessionStore>,
    artifacts: Arc<dyn CognitionArtifactStore>,
    invocation_id: CognitionInvocationId,
    revision: u64,
    response: ModelResponse,
    receipt_locator: String,
) -> Result<CognitionExecutionResult, CognitionExecutionError> {
    let revision = store
        .persist_cognition_receipt(CognitionReceiptPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            artifact_locator: receipt_locator,
        })
        .await
        .map_err(|_| CognitionExecutionError::Persistence)?;
    persist_output(store, artifacts, invocation_id, revision, response).await
}

async fn persist_output(
    store: Arc<dyn SessionStore>,
    artifacts: Arc<dyn CognitionArtifactStore>,
    invocation_id: CognitionInvocationId,
    revision: u64,
    response: ModelResponse,
) -> Result<CognitionExecutionResult, CognitionExecutionError> {
    let output = NormalizedCognitionOutput {
        schema_version: 1,
        invocation_id: invocation_id.as_str().to_owned(),
        provider_response_id: response.id.clone(),
        text: response.text(),
    };
    let artifact = artifacts
        .persist_exact(
            invocation_id.as_str(),
            CognitionArtifactKind::NormalizedOutput,
            "application/json",
            serde_json::to_vec(&output).map_err(|_| CognitionExecutionError::InvalidResponse)?,
        )
        .await
        .map_err(|_| CognitionExecutionError::Artifact)?;
    let revision = store
        .persist_cognition_output(CognitionOutputPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            artifact_locator: artifact.locator.clone(),
            output_digest: artifact.digest.clone(),
        })
        .await
        .map_err(|_| CognitionExecutionError::Persistence)?;
    store
        .complete_cognition(&invocation_id, revision)
        .await
        .map_err(|_| CognitionExecutionError::Persistence)?;
    Ok(CognitionExecutionResult {
        invocation_id,
        provider_response_id: response.id,
        text: output.text,
        artifact_locator: artifact.locator,
        output_digest: artifact.digest,
        usage: response.usage,
    })
}

async fn load_result(
    artifacts: &Arc<dyn CognitionArtifactStore>,
    snapshot: &CognitionInvocationSnapshot,
    response: ModelResponse,
) -> Result<CognitionExecutionResult, CognitionExecutionError> {
    let artifact = artifacts
        .load_exact(
            snapshot.invocation_id.as_str(),
            CognitionArtifactKind::NormalizedOutput,
        )
        .await
        .map_err(|_| CognitionExecutionError::Artifact)?
        .ok_or(CognitionExecutionError::Artifact)?;
    if snapshot.output_artifact_locator.as_deref() != Some(artifact.locator.as_str())
        || snapshot.output_digest.as_deref() != Some(artifact.digest.as_str())
    {
        return Err(CognitionExecutionError::InvalidResponse);
    }
    let output: NormalizedCognitionOutput = serde_json::from_slice(&artifact.payload)
        .map_err(|_| CognitionExecutionError::InvalidResponse)?;
    if output.schema_version != 1
        || output.invocation_id != snapshot.invocation_id.as_str()
        || output.provider_response_id != response.id
        || output.text.trim().is_empty()
    {
        return Err(CognitionExecutionError::InvalidResponse);
    }
    Ok(CognitionExecutionResult {
        invocation_id: snapshot.invocation_id.clone(),
        provider_response_id: response.id,
        text: output.text,
        artifact_locator: artifact.locator,
        output_digest: artifact.digest,
        usage: response.usage,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizedCognitionOutput {
    schema_version: u8,
    invocation_id: String,
    provider_response_id: String,
    text: String,
}

fn validate_request(request: &CognitionExecutionRequest) -> Result<(), CognitionExecutionError> {
    if request.turn_id.trim().is_empty()
        || request.prompt.trim().is_empty()
        || request.prompt.len() > MAX_COGNITION_PROMPT_BYTES
        || request.max_turn_calls == 0
        || !matches!(
            request.role,
            CognitiveRole::FastDraft | CognitiveRole::Deliberation | CognitiveRole::Critic
        )
    {
        return Err(CognitionExecutionError::InvalidRequest);
    }
    Ok(())
}

const fn failure_kind(error: CognitionExecutionError) -> CognitionFailureKind {
    match error {
        CognitionExecutionError::TimedOut => CognitionFailureKind::TimedOut,
        CognitionExecutionError::InvalidResponse => CognitionFailureKind::InvalidResponse,
        _ => CognitionFailureKind::Provider,
    }
}

fn role_instruction(role: CognitiveRole) -> &'static str {
    match role {
        CognitiveRole::FastDraft => {
            "Produce a concise candidate draft for the primary agent. Do not call tools or address the user."
        }
        CognitiveRole::Deliberation => {
            "Analyze the request carefully and return bounded reasoning and recommendations to the primary agent. Do not call tools or address the user."
        }
        CognitiveRole::Critic => {
            "Critique the proposed approach for correctness, safety, omissions, and cost. Return actionable findings to the primary agent. Do not call tools or address the user."
        }
        CognitiveRole::Vision | CognitiveRole::Audio | CognitiveRole::Document => {
            unreachable!("perception roles use the perception executor")
        }
    }
}

fn digest_prompt(prompt: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sylvander-cognition-prompt-v1\0");
    digest.update(prompt.len().to_be_bytes());
    digest.update(prompt.as_bytes());
    format!("sha256:{:x}", digest.finalize())
}

fn digest_capabilities(model: &ModelInfo) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sylvander-cognition-capabilities-v1\0");
    digest.update(model.reference.provider.as_bytes());
    digest.update(b"\0");
    digest.update(model.reference.model.as_bytes());
    digest.update(b"\0");
    digest.update(model.capabilities.bits().to_be_bytes());
    format!("sha256:{:x}", digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_roles_have_distinct_bounded_instructions() {
        assert_ne!(
            role_instruction(CognitiveRole::FastDraft),
            role_instruction(CognitiveRole::Critic)
        );
        assert!(role_instruction(CognitiveRole::Deliberation).contains("primary agent"));
    }
}
