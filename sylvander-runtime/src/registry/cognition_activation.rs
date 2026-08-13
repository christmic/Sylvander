//! Durable, owner-approved activation of auxiliary cognition routes.
//!
//! Benchmark evidence can make a route eligible, but only an authenticated
//! Registry administrator can turn that evidence into an active fact. Every
//! fact is bound to one immutable Agent revision, definition digest, role, and
//! exact model selection; revision changes therefore fail closed.

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sylvander_api::{AgentId, AuthenticatedPrincipal, ModelSelection};
use uuid::Uuid;

use crate::agent::cognition::CognitiveRole;
use crate::registry::administration::is_registry_administrator;
use crate::registry::agent::{AgentRegistry, AgentRegistryError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CognitionActivationEvidence {
    pub evidence_set_sha256: String,
    pub pairs: u32,
    pub minimum_pairs: u32,
    pub unsafe_candidates: u32,
    pub median_reward_gain_micros: i64,
    pub minimum_reward_gain_micros: i64,
    pub quality_win_basis_points: u16,
    pub minimum_quality_win_basis_points: u16,
    pub median_token_increase_basis_points: i32,
    pub maximum_token_increase_basis_points: u16,
    pub p95_latency_increase_basis_points: i32,
    pub maximum_p95_latency_increase_basis_points: u16,
}

impl CognitionActivationEvidence {
    fn validate(&self) -> Result<(), CognitionActivationError> {
        if !valid_sha256(&self.evidence_set_sha256)
            || self.minimum_pairs == 0
            || self.minimum_quality_win_basis_points > 10_000
            || self.quality_win_basis_points > 10_000
        {
            return Err(CognitionActivationError::InvalidEvidence);
        }
        if self.pairs < self.minimum_pairs
            || self.unsafe_candidates != 0
            || self.median_reward_gain_micros < self.minimum_reward_gain_micros
            || self.quality_win_basis_points < self.minimum_quality_win_basis_points
            || self.median_token_increase_basis_points
                > i32::from(self.maximum_token_increase_basis_points)
            || self.p95_latency_increase_basis_points
                > i32::from(self.maximum_p95_latency_increase_basis_points)
        {
            return Err(CognitionActivationError::IneligibleEvidence);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CognitionActivationDraft {
    pub agent_id: AgentId,
    pub agent_revision: u64,
    pub agent_definition_sha256: String,
    pub role: CognitiveRole,
    pub model: ModelSelection,
    pub evidence: CognitionActivationEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CognitionActivationState {
    Proposed,
    Approved,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CognitionActivationRecord {
    pub proposal_id: String,
    pub draft: CognitionActivationDraft,
    pub evidence_sha256: String,
    pub state: CognitionActivationState,
    pub state_revision: u64,
    pub proposed_by: String,
    pub approved_by: Option<String>,
    pub revoked_by: Option<String>,
}

impl AgentRegistry {
    pub async fn propose_cognition_activation(
        &self,
        principal: Option<&AuthenticatedPrincipal>,
        draft: CognitionActivationDraft,
    ) -> Result<CognitionActivationRecord, CognitionActivationError> {
        let actor = administrator(principal)?;
        draft.evidence.validate()?;
        if !valid_sha256(&draft.agent_definition_sha256) {
            return Err(CognitionActivationError::InvalidEvidence);
        }
        let evidence_json =
            serde_json::to_string(&draft.evidence).map_err(AgentRegistryError::serde)?;
        let evidence_sha256 = hex_digest(evidence_json.as_bytes());
        let proposal_id = Uuid::new_v4().to_string();
        let agent_revision = sql_u64(draft.agent_revision)?;
        let stored = self
            .load(&draft.agent_id, draft.agent_revision)
            .await?
            .ok_or(CognitionActivationError::UnknownAgentRevision)?;
        if stored.digest != draft.agent_definition_sha256 {
            return Err(CognitionActivationError::AgentDigestMismatch);
        }
        let binding = stored
            .definition
            .spec
            .cognition
            .binding(draft.role)
            .ok_or(CognitionActivationError::RoleNotConfigured)?;
        if binding.model != draft.model {
            return Err(CognitionActivationError::ModelBindingMismatch);
        }
        let record_draft = draft.clone();
        self.run_with(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(AgentRegistryError::sqlite)?;
            transaction
                .execute(
                    "INSERT INTO cognition_activation_proposals(\
                 proposal_id,agent_id,agent_revision,agent_digest,role,provider_id,model_id,\
                 evidence_json,evidence_digest,state,state_revision,proposed_by,proposed_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'proposed',1,?10,unixepoch())",
                    params![
                        proposal_id,
                        record_draft.agent_id.0,
                        agent_revision,
                        record_draft.agent_definition_sha256,
                        role_str(record_draft.role),
                        record_draft.model.provider_id,
                        record_draft.model.model_id,
                        evidence_json,
                        evidence_sha256,
                        actor
                    ],
                )
                .map_err(AgentRegistryError::sqlite)?;
            append_event(&transaction, &proposal_id, 1, "proposed", &actor)?;
            transaction.commit().map_err(AgentRegistryError::sqlite)?;
            Ok(CognitionActivationRecord {
                proposal_id,
                draft: record_draft,
                evidence_sha256,
                state: CognitionActivationState::Proposed,
                state_revision: 1,
                proposed_by: actor,
                approved_by: None,
                revoked_by: None,
            })
        })
        .await
    }

    pub async fn approve_cognition_activation(
        &self,
        principal: Option<&AuthenticatedPrincipal>,
        proposal_id: &str,
        expected_state_revision: u64,
    ) -> Result<CognitionActivationRecord, CognitionActivationError> {
        transition(self, principal, proposal_id, expected_state_revision, false).await
    }

    pub async fn revoke_cognition_activation(
        &self,
        principal: Option<&AuthenticatedPrincipal>,
        proposal_id: &str,
        expected_state_revision: u64,
    ) -> Result<CognitionActivationRecord, CognitionActivationError> {
        transition(self, principal, proposal_id, expected_state_revision, true).await
    }

    pub async fn active_cognition_activation(
        &self,
        agent_id: &AgentId,
        revision: u64,
        role: CognitiveRole,
    ) -> Result<Option<CognitionActivationRecord>, CognitionActivationError> {
        let agent_id = agent_id.0.clone();
        let revision = sql_u64(revision)?;
        let record = self
            .run_with(move |connection| {
                let id = connection
                    .query_row(
                        "SELECT proposal_id FROM cognition_activation_proposals \
                 WHERE agent_id=?1 AND agent_revision=?2 AND role=?3 AND state='approved'",
                        params![agent_id, revision, role_str(role)],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(AgentRegistryError::sqlite)?;
                id.map_or(Ok(None), |id| load_record(connection, &id).map(Some))
            })
            .await?;
        let Some(record) = record else {
            return Ok(None);
        };
        let stored = self
            .load(&record.draft.agent_id, record.draft.agent_revision)
            .await?
            .ok_or(CognitionActivationError::UnknownAgentRevision)?;
        let binding = stored
            .definition
            .spec
            .cognition
            .binding(record.draft.role)
            .ok_or(CognitionActivationError::RoleNotConfigured)?;
        if stored.digest != record.draft.agent_definition_sha256 {
            return Err(CognitionActivationError::AgentDigestMismatch);
        }
        if binding.model != record.draft.model {
            return Err(CognitionActivationError::ModelBindingMismatch);
        }
        Ok(Some(record))
    }
}

async fn transition(
    registry: &AgentRegistry,
    principal: Option<&AuthenticatedPrincipal>,
    proposal_id: &str,
    expected: u64,
    revoke: bool,
) -> Result<CognitionActivationRecord, CognitionActivationError> {
    let actor = administrator(principal)?;
    let proposal_id = proposal_id.to_owned();
    registry.run_with(move |connection| {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(AgentRegistryError::sqlite)?;
        let current = load_record(&transaction, &proposal_id)?;
        if current.state_revision != expected {
            return Err(CognitionActivationError::Conflict);
        }
        let expected_state = if revoke { CognitionActivationState::Approved } else { CognitionActivationState::Proposed };
        if current.state != expected_state {
            return Err(CognitionActivationError::InvalidTransition);
        }
        let next = expected.checked_add(1).ok_or(CognitionActivationError::Conflict)?;
        let next_sql = sql_u64(next)?;
        let (state, actor_column, time_column) = if revoke {
            ("revoked", "revoked_by", "revoked_at")
        } else {
            ("approved", "approved_by", "approved_at")
        };
        let sql = format!("UPDATE cognition_activation_proposals SET state=?2,state_revision=?3,{actor_column}=?4,{time_column}=unixepoch() WHERE proposal_id=?1");
        transaction.execute(&sql, params![proposal_id, state, next_sql, actor])
            .map_err(map_transition_storage)?;
        append_event(&transaction, &proposal_id, next, state, &actor)?;
        transaction.commit().map_err(AgentRegistryError::sqlite)?;
        load_record(connection, &proposal_id)
    }).await
}

fn administrator(
    principal: Option<&AuthenticatedPrincipal>,
) -> Result<String, CognitionActivationError> {
    if !is_registry_administrator(principal) {
        return Err(CognitionActivationError::Unauthorized);
    }
    Ok(principal
        .expect("administrator is authenticated")
        .id
        .0
        .clone())
}

fn append_event(
    transaction: &rusqlite::Transaction<'_>,
    id: &str,
    revision: u64,
    event: &str,
    actor: &str,
) -> Result<(), CognitionActivationError> {
    let revision = sql_u64(revision)?;
    transaction.execute(
        "INSERT INTO cognition_activation_events(proposal_id,state_revision,event,actor,occurred_at) VALUES (?1,?2,?3,?4,unixepoch())",
        params![id, revision, event, actor],
    ).map_err(AgentRegistryError::sqlite)?;
    Ok(())
}

fn load_record(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<CognitionActivationRecord, CognitionActivationError> {
    let row = connection.query_row(
        "SELECT agent_id,agent_revision,agent_digest,role,provider_id,model_id,evidence_json,evidence_digest,state,state_revision,proposed_by,approved_by,revoked_by FROM cognition_activation_proposals WHERE proposal_id=?1",
        [id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?, row.get::<_, String>(6)?, row.get::<_, String>(7)?, row.get::<_, String>(8)?, row.get::<_, i64>(9)?, row.get::<_, String>(10)?, row.get::<_, Option<String>>(11)?, row.get::<_, Option<String>>(12)?)),
    ).optional().map_err(AgentRegistryError::sqlite)?.ok_or(CognitionActivationError::UnknownProposal)?;
    let evidence: CognitionActivationEvidence =
        serde_json::from_str(&row.6).map_err(AgentRegistryError::serde)?;
    evidence.validate()?;
    if hex_digest(row.6.as_bytes()) != row.7 {
        return Err(CognitionActivationError::Integrity);
    }
    let state = parse_state(&row.8)?;
    if !valid_state_fact(state, row.9, row.11.as_deref(), row.12.as_deref()) {
        return Err(CognitionActivationError::Integrity);
    }
    Ok(CognitionActivationRecord {
        proposal_id: id.to_owned(),
        draft: CognitionActivationDraft {
            agent_id: AgentId(row.0),
            agent_revision: decode_u64(row.1)?,
            agent_definition_sha256: row.2,
            role: parse_role(&row.3)?,
            model: ModelSelection {
                provider_id: row.4,
                model_id: row.5,
            },
            evidence,
        },
        evidence_sha256: row.7,
        state,
        state_revision: decode_u64(row.9)?,
        proposed_by: row.10,
        approved_by: row.11,
        revoked_by: row.12,
    })
}

const fn valid_state_fact(
    state: CognitionActivationState,
    revision: i64,
    approved_by: Option<&str>,
    revoked_by: Option<&str>,
) -> bool {
    match state {
        CognitionActivationState::Proposed => {
            revision == 1 && approved_by.is_none() && revoked_by.is_none()
        }
        CognitionActivationState::Approved => {
            revision == 2 && approved_by.is_some() && revoked_by.is_none()
        }
        CognitionActivationState::Revoked => {
            revision == 3 && approved_by.is_some() && revoked_by.is_some()
        }
    }
}

fn map_transition_storage(error: rusqlite::Error) -> CognitionActivationError {
    if matches!(error, rusqlite::Error::SqliteFailure(ref code, _) if code.extended_code == 2067) {
        CognitionActivationError::AlreadyApproved
    } else {
        AgentRegistryError::sqlite(error).into()
    }
}

const fn role_str(role: CognitiveRole) -> &'static str {
    match role {
        CognitiveRole::FastDraft => "fast_draft",
        CognitiveRole::Deliberation => "deliberation",
        CognitiveRole::Critic => "critic",
        CognitiveRole::Vision => "vision",
        CognitiveRole::Audio => "audio",
        CognitiveRole::Document => "document",
    }
}
fn parse_role(value: &str) -> Result<CognitiveRole, CognitionActivationError> {
    match value {
        "fast_draft" => Ok(CognitiveRole::FastDraft),
        "deliberation" => Ok(CognitiveRole::Deliberation),
        "critic" => Ok(CognitiveRole::Critic),
        "vision" => Ok(CognitiveRole::Vision),
        "audio" => Ok(CognitiveRole::Audio),
        "document" => Ok(CognitiveRole::Document),
        _ => Err(CognitionActivationError::Integrity),
    }
}
fn parse_state(value: &str) -> Result<CognitionActivationState, CognitionActivationError> {
    match value {
        "proposed" => Ok(CognitionActivationState::Proposed),
        "approved" => Ok(CognitionActivationState::Approved),
        "revoked" => Ok(CognitionActivationState::Revoked),
        _ => Err(CognitionActivationError::Integrity),
    }
}
fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn sql_u64(value: u64) -> Result<i64, CognitionActivationError> {
    i64::try_from(value).map_err(|_| CognitionActivationError::InvalidEvidence)
}

fn decode_u64(value: i64) -> Result<u64, CognitionActivationError> {
    u64::try_from(value).map_err(|_| CognitionActivationError::Integrity)
}

#[derive(Debug, thiserror::Error)]
pub enum CognitionActivationError {
    #[error("cognition activation requires a Registry administrator")]
    Unauthorized,
    #[error("cognition evidence is invalid")]
    InvalidEvidence,
    #[error("cognition evidence did not satisfy its activation policy")]
    IneligibleEvidence,
    #[error("unknown Agent revision")]
    UnknownAgentRevision,
    #[error("Agent definition digest does not match")]
    AgentDigestMismatch,
    #[error("cognitive role is not configured on this Agent revision")]
    RoleNotConfigured,
    #[error("cognitive model does not match the Agent role binding")]
    ModelBindingMismatch,
    #[error("unknown cognition activation proposal")]
    UnknownProposal,
    #[error("cognition activation state changed concurrently")]
    Conflict,
    #[error("cognition activation transition is invalid")]
    InvalidTransition,
    #[error("an approved activation already exists for this Agent role")]
    AlreadyApproved,
    #[error("cognition activation record failed integrity validation")]
    Integrity,
    #[error(transparent)]
    Registry(#[from] AgentRegistryError),
}

#[cfg(test)]
#[path = "../../tests/unit/cognition_activation.rs"]
mod tests;
