//! Durable execution sandwich for one same-Agent perception specialist.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sylvander_api::AgentInstanceId;
use sylvander_llm_core::{
    AudioFormat, ChatMessage, ContentBlock, MediaSource, ModelInfo, ModelProvider, ModelRef,
    ModelRequest, ModelResponse, ModelStreamEvent, StopReason, SystemInstruction, TokenUsage,
    validate_model_request_capabilities,
};

use super::cognition::CognitiveRole;
use super::perception::{PerceptionArtifactKind, PerceptionArtifactStore, PerceptionModality};
use crate::agent_definition::SessionId;
use crate::storage::session::{
    PerceptionAdvance, PerceptionArtifactPersistence, PerceptionExecutionPosition,
    PerceptionInvocationId, PerceptionInvocationSnapshot, PerceptionInvocationStart,
    PerceptionMediaPersistence, PerceptionReceiptPersistence, PerceptionRecoveryPolicy,
    SessionStore,
};

const PERCEPTION_TIMEOUT: Duration = Duration::from_mins(2);
const MAX_SPECIALIST_OUTPUT_TOKENS: u32 = 4_096;

/// Immutable input for one specialist call. The media block and raw bytes must
/// describe the same payload; Runtime ingress constructs both from one decoded
/// attachment.
#[derive(Clone)]
pub struct PerceptionExecutionRequest {
    pub session_id: SessionId,
    pub turn_id: String,
    pub agent_instance_id: AgentInstanceId,
    pub invocation_id: PerceptionInvocationId,
    pub modality: PerceptionModality,
    pub role: CognitiveRole,
    pub model: ModelInfo,
    pub recovery_policy: PerceptionRecoveryPolicy,
    pub media_type: String,
    pub media_bytes: Vec<u8>,
    pub media_block: ContentBlock,
}

/// Explicit input for the evidence-gathering path. Runtime does not invoke
/// this path automatically; benchmark or evaluation code must hold an
/// authenticated Session capability and choose one configured role.
#[derive(Clone)]
pub struct PerceptionEvaluationInput {
    pub turn_id: String,
    pub invocation_id: PerceptionInvocationId,
    pub modality: PerceptionModality,
    pub role: CognitiveRole,
    pub recovery_policy: PerceptionRecoveryPolicy,
    pub media_type: String,
    pub media_bytes: Vec<u8>,
    pub media_block: ContentBlock,
}

/// Bounded model-visible result returned to the primary model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerceptionExecutionResult {
    pub invocation_id: PerceptionInvocationId,
    pub provider_response_id: String,
    pub text: String,
    pub artifact_locator: String,
    pub output_digest: String,
    pub usage: TokenUsage,
}

/// Content-safe specialist failure. Durable position remains authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PerceptionExecutionError {
    #[error("perception evaluation is not authorized")]
    Unauthorized,
    #[error("perception specialist is not configured")]
    SpecialistNotConfigured,
    #[error("perception execution infrastructure is unavailable")]
    Unavailable,
    #[error("perception request is invalid")]
    InvalidRequest,
    #[error("perception model is incompatible")]
    IncompatibleModel,
    #[error("perception provider failed")]
    Provider,
    #[error("perception provider timed out")]
    TimedOut,
    #[error("perception provider response is invalid")]
    InvalidResponse,
    #[error("perception durable state is unavailable")]
    Persistence,
    #[error("perception encrypted artifact is unavailable")]
    Artifact,
    #[error("perception receipt does not exist")]
    ReceiptMissing,
}

/// Execute one fresh specialist call. No provider retry occurs inside this
/// function; uncertainty is resolved only through the declared recovery policy.
pub async fn execute_perception(
    store: Arc<dyn SessionStore>,
    artifacts: Arc<dyn PerceptionArtifactStore>,
    provider: Arc<dyn ModelProvider>,
    request: PerceptionExecutionRequest,
) -> Result<PerceptionExecutionResult, PerceptionExecutionError> {
    validate_request(&request)?;
    let input_digest = digest_input(&request.media_type, &request.media_bytes);
    let capability_revision = digest_capabilities(&request.model);
    store
        .begin_perception(PerceptionInvocationStart {
            session_id: request.session_id.clone(),
            turn_id: request.turn_id.clone(),
            agent_instance_id: request.agent_instance_id.clone(),
            invocation_id: request.invocation_id.clone(),
            modality: request.modality,
            role: request.role,
            provider_id: request.model.reference.provider.clone(),
            model_id: request.model.reference.model.clone(),
            recovery_policy: request.recovery_policy,
            capability_revision,
            input_digest,
            input_bytes: u64::try_from(request.media_bytes.len())
                .map_err(|_| PerceptionExecutionError::InvalidRequest)?,
        })
        .await
        .map_err(|_| PerceptionExecutionError::Persistence)?;
    let media = artifacts
        .persist_exact(
            &request.invocation_id,
            PerceptionArtifactKind::SourceMedia,
            &request.media_type,
            request.media_bytes.clone(),
        )
        .await
        .map_err(|_| PerceptionExecutionError::Artifact)?;
    let revision = store
        .persist_perception_media(PerceptionMediaPersistence {
            invocation_id: request.invocation_id.clone(),
            expected_revision: 0,
            artifact_locator: media.locator,
        })
        .await
        .map_err(|_| PerceptionExecutionError::Persistence)?;
    let revision = store
        .advance_perception(PerceptionAdvance {
            invocation_id: request.invocation_id.clone(),
            expected_revision: revision,
            expected_position: PerceptionExecutionPosition::MediaPersisted,
            next_position: PerceptionExecutionPosition::InferenceStarted,
        })
        .await
        .map_err(|_| PerceptionExecutionError::Persistence)?;
    let response = call_specialist(provider.as_ref(), &request).await?;
    finish_from_response(store, artifacts, request.invocation_id, revision, response).await
}

/// Resume after a deterministic provider receipt was written but the `SQLite`
/// program counter did not cross `InferenceCompleted`.
pub async fn recover_perception_receipt(
    store: Arc<dyn SessionStore>,
    artifacts: Arc<dyn PerceptionArtifactStore>,
    snapshot: PerceptionInvocationSnapshot,
) -> Result<PerceptionExecutionResult, PerceptionExecutionError> {
    if snapshot.position != PerceptionExecutionPosition::InferenceStarted
        || snapshot.recovery_policy != PerceptionRecoveryPolicy::RecoverFromReceipt
    {
        return Err(PerceptionExecutionError::InvalidRequest);
    }
    let receipt = artifacts
        .load_exact(
            &snapshot.invocation_id,
            PerceptionArtifactKind::ProviderReceipt,
        )
        .await
        .map_err(|_| PerceptionExecutionError::Artifact)?
        .ok_or(PerceptionExecutionError::ReceiptMissing)?;
    let response: ModelResponse = serde_json::from_slice(&receipt.payload)
        .map_err(|_| PerceptionExecutionError::InvalidResponse)?;
    if response.model.provider != snapshot.provider_id || response.model.model != snapshot.model_id
    {
        return Err(PerceptionExecutionError::InvalidResponse);
    }
    finish_from_persisted_receipt(
        store,
        artifacts,
        snapshot.invocation_id,
        snapshot.ledger_revision,
        response,
        receipt.locator,
    )
    .await
}

async fn call_specialist(
    provider: &dyn ModelProvider,
    request: &PerceptionExecutionRequest,
) -> Result<ModelResponse, PerceptionExecutionError> {
    let model_request = ModelRequest {
        request_id: request.invocation_id.as_str().to_owned(),
        model: request.model.reference.clone(),
        system: vec![SystemInstruction {
            text: specialist_instruction(request.modality).to_owned(),
            cache_hint: None,
        }],
        messages: vec![ChatMessage::user_blocks(vec![request.media_block.clone()])],
        tools: Vec::new(),
        max_output_tokens: request
            .model
            .max_output_tokens
            .min(MAX_SPECIALIST_OUTPUT_TOKENS),
        reasoning: None,
        output_schema: None,
    };
    validate_model_request_capabilities(&model_request, request.model.capabilities)
        .map_err(|_| PerceptionExecutionError::IncompatibleModel)?;
    let stream = tokio::time::timeout(PERCEPTION_TIMEOUT, provider.complete_stream(model_request))
        .await
        .map_err(|_| PerceptionExecutionError::TimedOut)?
        .map_err(|_| PerceptionExecutionError::Provider)?;
    consume_response(stream, &request.model.reference).await
}

async fn consume_response(
    mut stream: sylvander_llm_core::ModelEventStream,
    expected: &ModelRef,
) -> Result<ModelResponse, PerceptionExecutionError> {
    let mut completed = None;
    loop {
        let next = tokio::time::timeout(PERCEPTION_TIMEOUT, stream.next())
            .await
            .map_err(|_| PerceptionExecutionError::TimedOut)?;
        let Some(event) = next else { break };
        let event = event.map_err(|_| PerceptionExecutionError::Provider)?;
        if completed.is_some() {
            return Err(PerceptionExecutionError::InvalidResponse);
        }
        if let ModelStreamEvent::Completed(response) = event {
            completed = Some(*response);
        }
    }
    let response = completed.ok_or(PerceptionExecutionError::InvalidResponse)?;
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
        return Err(PerceptionExecutionError::InvalidResponse);
    }
    Ok(response)
}

async fn finish_from_response(
    store: Arc<dyn SessionStore>,
    artifacts: Arc<dyn PerceptionArtifactStore>,
    invocation_id: PerceptionInvocationId,
    revision: u64,
    response: ModelResponse,
) -> Result<PerceptionExecutionResult, PerceptionExecutionError> {
    let receipt_payload =
        serde_json::to_vec(&response).map_err(|_| PerceptionExecutionError::InvalidResponse)?;
    let receipt = artifacts
        .persist_exact(
            &invocation_id,
            PerceptionArtifactKind::ProviderReceipt,
            "application/json",
            receipt_payload,
        )
        .await
        .map_err(|_| PerceptionExecutionError::Artifact)?;
    finish_from_persisted_receipt(
        store,
        artifacts,
        invocation_id,
        revision,
        response,
        receipt.locator,
    )
    .await
}

async fn finish_from_persisted_receipt(
    store: Arc<dyn SessionStore>,
    artifacts: Arc<dyn PerceptionArtifactStore>,
    invocation_id: PerceptionInvocationId,
    revision: u64,
    response: ModelResponse,
    receipt_locator: String,
) -> Result<PerceptionExecutionResult, PerceptionExecutionError> {
    let revision = store
        .persist_perception_receipt(PerceptionReceiptPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            receipt_locator,
        })
        .await
        .map_err(|_| PerceptionExecutionError::Persistence)?;
    let text = response.text();
    let output = NormalizedPerceptionOutput {
        schema_version: 1,
        invocation_id: invocation_id.as_str(),
        provider_response_id: &response.id,
        text: &text,
    };
    let output_payload =
        serde_json::to_vec(&output).map_err(|_| PerceptionExecutionError::InvalidResponse)?;
    let artifact = artifacts
        .persist_exact(
            &invocation_id,
            PerceptionArtifactKind::NormalizedOutput,
            "application/json",
            output_payload,
        )
        .await
        .map_err(|_| PerceptionExecutionError::Artifact)?;
    let revision = store
        .persist_perception_artifact(PerceptionArtifactPersistence {
            invocation_id: invocation_id.clone(),
            expected_revision: revision,
            artifact_locator: artifact.locator.clone(),
            output_digest: artifact.digest.clone(),
        })
        .await
        .map_err(|_| PerceptionExecutionError::Persistence)?;
    store
        .complete_perception(&invocation_id, revision)
        .await
        .map_err(|_| PerceptionExecutionError::Persistence)?;
    Ok(PerceptionExecutionResult {
        invocation_id,
        provider_response_id: response.id,
        text,
        artifact_locator: artifact.locator,
        output_digest: artifact.digest,
        usage: response.usage,
    })
}

#[derive(Serialize)]
struct NormalizedPerceptionOutput<'a> {
    schema_version: u8,
    invocation_id: &'a str,
    provider_response_id: &'a str,
    text: &'a str,
}

fn validate_request(request: &PerceptionExecutionRequest) -> Result<(), PerceptionExecutionError> {
    if request.turn_id.trim().is_empty()
        || request.media_type.trim().is_empty()
        || request.media_bytes.is_empty()
        || !role_matches_modality(request.role, request.modality)
        || !block_matches_input(
            &request.media_block,
            request.modality,
            &request.media_type,
            &request.media_bytes,
        )
    {
        return Err(PerceptionExecutionError::InvalidRequest);
    }
    Ok(())
}

const fn role_matches_modality(role: CognitiveRole, modality: PerceptionModality) -> bool {
    matches!(
        (role, modality),
        (CognitiveRole::Vision, PerceptionModality::Image)
            | (CognitiveRole::Audio, PerceptionModality::Audio)
            | (CognitiveRole::Document, PerceptionModality::Document)
    )
}

fn block_matches_input(
    block: &ContentBlock,
    modality: PerceptionModality,
    media_type: &str,
    bytes: &[u8],
) -> bool {
    let encoded = match (block, modality) {
        (ContentBlock::Image { image }, PerceptionModality::Image) => match &image.source {
            MediaSource::Base64 {
                media_type: block_type,
                data,
            } if block_type == media_type => data,
            MediaSource::Base64 { .. } | MediaSource::Url { .. } => return false,
        },
        (ContentBlock::Document { document }, PerceptionModality::Document) => {
            match &document.source {
                MediaSource::Base64 {
                    media_type: block_type,
                    data,
                } if block_type == media_type => data,
                MediaSource::Base64 { .. } | MediaSource::Url { .. } => return false,
            }
        }
        (ContentBlock::Audio { audio }, PerceptionModality::Audio)
            if matches!(
                (audio.format, media_type),
                (AudioFormat::Wav, "audio/wav" | "audio/x-wav") | (AudioFormat::Mp3, "audio/mpeg")
            ) =>
        {
            &audio.data
        }
        _ => return false,
    };
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .is_ok_and(|decoded| decoded == bytes)
}

const fn specialist_instruction(modality: PerceptionModality) -> &'static str {
    match modality {
        PerceptionModality::Image => {
            "Describe only the observable image content needed by the primary agent. Do not call tools."
        }
        PerceptionModality::Audio => {
            "Transcribe and describe only the observable audio content needed by the primary agent. Do not call tools."
        }
        PerceptionModality::Document => {
            "Extract only the observable document content needed by the primary agent. Do not call tools."
        }
    }
}

fn digest_input(media_type: &str, bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sylvander-perception-input-v1\0");
    digest.update(media_type.len().to_be_bytes());
    digest.update(media_type.as_bytes());
    digest.update(bytes.len().to_be_bytes());
    digest.update(bytes);
    format!("sha256:{:x}", digest.finalize())
}

fn digest_capabilities(model: &ModelInfo) -> String {
    let mut digest = Sha256::new();
    digest.update(b"sylvander-perception-capabilities-v1\0");
    digest.update(model.reference.provider.as_bytes());
    digest.update(b"\0");
    digest.update(model.reference.model.as_bytes());
    digest.update(b"\0");
    digest.update(model.capabilities.bits().to_be_bytes());
    format!("sha256:{:x}", digest.finalize())
}
