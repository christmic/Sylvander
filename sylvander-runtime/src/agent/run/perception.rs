//! Authenticated execution and recovery for same-Agent perception.

use std::sync::Arc;

use super::{AgentRun, AgentRunInner, AuthenticatedSession};
use crate::agent::perception_execution::{
    PerceptionEvaluationInput, PerceptionExecutionError, PerceptionExecutionRequest,
    PerceptionExecutionResult, PerceptionInvocationId, execute_perception,
    recover_perception_receipt,
};
use crate::observability::RuntimeEvent;
use crate::session::{AgentSessionKey, now_secs};
use crate::storage::artifact::ArtifactTurnBinding;

impl AgentRun {
    /// Execute one explicitly requested specialist for paired evaluation.
    pub async fn evaluate_perception_specialist(
        &self,
        session: &AuthenticatedSession,
        input: PerceptionEvaluationInput,
    ) -> Result<PerceptionExecutionResult, PerceptionExecutionError> {
        if !Arc::ptr_eq(&self.inner.session_authority, &session.authority) {
            return Err(PerceptionExecutionError::Unauthorized);
        }
        self.inner
            .execute_perception_specialist(session, input, false)
            .await
    }

    /// Resume a configured specialist from durable post-inference artifacts.
    /// This path never invokes a model.
    pub async fn recover_perception_specialist(
        &self,
        session: &AuthenticatedSession,
        turn_id: &str,
        invocation_id: &PerceptionInvocationId,
    ) -> Result<PerceptionExecutionResult, PerceptionExecutionError> {
        if !Arc::ptr_eq(&self.inner.session_authority, &session.authority) {
            return Err(PerceptionExecutionError::Unauthorized);
        }
        let store = self
            .inner
            .session_store
            .clone()
            .ok_or(PerceptionExecutionError::Unavailable)?;
        let snapshot = store
            .perception_invocations(&session.session_id, turn_id)
            .await
            .map_err(|_| PerceptionExecutionError::Persistence)?
            .into_iter()
            .find(|snapshot| {
                snapshot.invocation_id == *invocation_id
                    && snapshot.agent_instance_id == session.agent_instance_id
            })
            .ok_or(PerceptionExecutionError::Unauthorized)?;
        let binding = self
            .inner
            .spec
            .cognition
            .binding(snapshot.role)
            .ok_or(PerceptionExecutionError::SpecialistNotConfigured)?;
        if binding.model.provider_id != snapshot.provider_id
            || binding.model.model_id != snapshot.model_id
        {
            return Err(PerceptionExecutionError::SpecialistNotConfigured);
        }
        let metadata = self.inner.session_metadata(session).await?;
        let artifacts = self
            .inner
            .bind_cognition_artifacts(session, &metadata, turn_id)?;
        let result = recover_perception_receipt(store, artifacts, snapshot).await;
        self.inner.record_perception_terminal(
            session,
            turn_id.to_owned(),
            invocation_id,
            &result,
            true,
            false,
        );
        result
    }
}

impl AgentRunInner {
    pub(super) async fn execute_perception_specialist(
        &self,
        session: &AuthenticatedSession,
        input: PerceptionEvaluationInput,
        automatic: bool,
    ) -> Result<PerceptionExecutionResult, PerceptionExecutionError> {
        let binding = self
            .spec
            .cognition
            .binding(input.role)
            .ok_or(PerceptionExecutionError::SpecialistNotConfigured)?;
        let model = self
            .runtime_models
            .read()
            .await
            .available
            .get(&binding.model)
            .and_then(|model| model.exact.clone())
            .ok_or(PerceptionExecutionError::SpecialistNotConfigured)?;
        let metadata = self.session_metadata(session).await?;
        let store = self
            .session_store
            .clone()
            .ok_or(PerceptionExecutionError::Unavailable)?;
        let artifacts = self.bind_cognition_artifacts(session, &metadata, &input.turn_id)?;
        let turn_id = input.turn_id.clone();
        let invocation_id = input.invocation_id.clone();
        let result = execute_perception(
            store,
            artifacts,
            self.model_provider.clone(),
            PerceptionExecutionRequest {
                session_id: session.session_id.clone(),
                turn_id: input.turn_id,
                agent_instance_id: session.agent_instance_id.clone(),
                invocation_id: input.invocation_id,
                modality: input.modality,
                role: input.role,
                model,
                recovery_policy: input.recovery_policy,
                media_type: input.media_type,
                media_bytes: input.media_bytes,
                media_block: input.media_block,
            },
        )
        .await;
        self.record_perception_terminal(
            session,
            turn_id,
            &invocation_id,
            &result,
            false,
            automatic,
        );
        result
    }

    async fn session_metadata(
        &self,
        session: &AuthenticatedSession,
    ) -> Result<crate::session::SessionMetadata, PerceptionExecutionError> {
        self.sessions
            .read()
            .await
            .get(&AgentSessionKey::new(
                session.session_id.clone(),
                session.agent_instance_id.clone(),
            ))
            .map(|context| context.metadata.clone())
            .ok_or(PerceptionExecutionError::Unauthorized)
    }

    pub(super) fn bind_cognition_artifacts(
        &self,
        session: &AuthenticatedSession,
        metadata: &crate::session::SessionMetadata,
        turn_id: &str,
    ) -> Result<
        Arc<dyn crate::agent::cognition_artifact::CognitionArtifactStore>,
        PerceptionExecutionError,
    > {
        self.artifact_service
            .as_ref()
            .ok_or(PerceptionExecutionError::Unavailable)?
            .bind_cognition(ArtifactTurnBinding {
                user_id: metadata.user_id.clone(),
                agent_id: self.id.0.clone(),
                session_id: session.session_id.0.clone(),
                turn_id: turn_id.to_owned(),
                created_at: now_secs(),
            })
            .map_err(|_| PerceptionExecutionError::Unavailable)
    }

    fn record_perception_terminal(
        &self,
        session: &AuthenticatedSession,
        turn_id: String,
        invocation_id: &PerceptionInvocationId,
        result: &Result<PerceptionExecutionResult, PerceptionExecutionError>,
        recovered_from_receipt: bool,
        automatic: bool,
    ) {
        self.observability
            .record(RuntimeEvent::PerceptionEvaluationFinished {
                turn_id,
                session_id: session.session_id.clone(),
                invocation_id: invocation_id.as_str().to_owned(),
                succeeded: result.is_ok(),
                recovered_from_receipt,
                automatic,
            });
    }
}
