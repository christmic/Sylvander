use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};
use sylvander_protocol::EvidenceReference;

use super::evaluation::{digest_text, valid_key, valid_sha256};
use super::proposal_types::{
    ImprovementProposal, ImprovementProposalStatus, ImprovementRisk, RequiredEvaluation,
};
use super::{EvidenceError, EvidenceStore, as_i64};

mod read;

impl EvidenceStore {
    /// Persist one immutable evidence-linked proposal in the draft state.
    pub async fn register_improvement_proposal(
        &self,
        definition: ImprovementProposal,
    ) -> Result<String, EvidenceError> {
        validate_proposal(&definition)?;
        let mut evidence = definition.evidence.clone();
        evidence.sort_by(|left, right| left.locator.cmp(&right.locator));
        let mut components = definition.affected_components.clone();
        components.sort();
        let mut evaluations = definition.required_evaluations.clone();
        evaluations.sort_by(|left, right| {
            (&left.dataset_id, left.dataset_revision, &left.baseline_id).cmp(&(
                &right.dataset_id,
                right.dataset_revision,
                &right.baseline_id,
            ))
        });
        let digest = proposal_digest(&definition, &evidence, &components, &evaluations);
        let stored_digest = digest.clone();
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let existing = transaction
                .query_row(
                    "SELECT definition_digest_sha256
                     FROM evidence_improvement_proposals WHERE id=?1",
                    [&definition.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(EvidenceError::sqlite)?;
            if let Some(existing) = existing {
                return if existing == stored_digest {
                    Ok(())
                } else {
                    Err(EvidenceError::ProposalStateConflict)
                };
            }
            for required in &evaluations {
                let valid = transaction
                    .query_row(
                        "SELECT EXISTS(
                           SELECT 1 FROM evidence_evaluation_baselines
                           WHERE id=?1 AND dataset_id=?2 AND dataset_revision=?3
                         )",
                        params![
                            required.baseline_id,
                            required.dataset_id,
                            as_i64(required.dataset_revision)?
                        ],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(EvidenceError::sqlite)?;
                if !valid {
                    return Err(EvidenceError::InvalidImprovementProposal);
                }
            }
            transaction
                .execute(
                    "INSERT INTO evidence_improvement_proposals(
                       id, cohort_digest_sha256, hypothesis, expected_benefit,
                       risk, rollback_plan, created_by_principal_digest,
                       created_at, definition_digest_sha256, status, state_revision
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'draft', 1)",
                    params![
                        definition.id,
                        definition.cohort_digest_sha256,
                        definition.hypothesis,
                        definition.expected_benefit,
                        risk_name(definition.risk),
                        definition.rollback_plan,
                        definition.created_by_principal_digest,
                        definition.created_at,
                        stored_digest,
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            for (position, reference) in evidence.into_iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO evidence_improvement_proposal_evidence(
                           proposal_id, position, locator, digest_sha256
                         ) VALUES (?1, ?2, ?3, ?4)",
                        params![
                            definition.id,
                            as_i64(position as u64)?,
                            reference.locator,
                            reference.digest_sha256
                        ],
                    )
                    .map_err(EvidenceError::sqlite)?;
            }
            for (position, component) in components.into_iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO evidence_improvement_proposal_components(
                           proposal_id, position, component
                         ) VALUES (?1, ?2, ?3)",
                        params![definition.id, as_i64(position as u64)?, component],
                    )
                    .map_err(EvidenceError::sqlite)?;
            }
            for (position, required) in evaluations.into_iter().enumerate() {
                transaction
                    .execute(
                        "INSERT INTO evidence_improvement_proposal_evaluations(
                           proposal_id, position, dataset_id, dataset_revision, baseline_id
                         ) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            definition.id,
                            as_i64(position as u64)?,
                            required.dataset_id,
                            as_i64(required.dataset_revision)?,
                            required.baseline_id
                        ],
                    )
                    .map_err(EvidenceError::sqlite)?;
            }
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await?;
        Ok(digest)
    }
}

fn validate_proposal(definition: &ImprovementProposal) -> Result<(), EvidenceError> {
    if !valid_key(&definition.id)
        || !valid_sha256(&definition.cohort_digest_sha256)
        || !valid_text(&definition.hypothesis)
        || !valid_text(&definition.expected_benefit)
        || !valid_text(&definition.rollback_plan)
        || !valid_sha256(&definition.created_by_principal_digest)
        || definition.created_at < 0
        || definition.evidence.is_empty()
        || definition.evidence.len() > 32
        || definition.affected_components.is_empty()
        || definition.affected_components.len() > 32
        || definition.required_evaluations.is_empty()
        || definition.required_evaluations.len() > 32
    {
        return Err(EvidenceError::InvalidImprovementProposal);
    }
    let mut locators = BTreeSet::new();
    if definition.evidence.iter().any(|reference| {
        reference.locator.is_empty()
            || reference.locator.len() > 1024
            || !locators.insert(&reference.locator)
            || !reference.digest_sha256.as_deref().is_some_and(valid_sha256)
    }) {
        return Err(EvidenceError::InvalidImprovementProposal);
    }
    let mut components = BTreeSet::new();
    if definition
        .affected_components
        .iter()
        .any(|component| !valid_key(component) || !components.insert(component))
    {
        return Err(EvidenceError::InvalidImprovementProposal);
    }
    let mut evaluations = BTreeSet::new();
    if definition.required_evaluations.iter().any(|required| {
        !valid_key(&required.dataset_id)
            || required.dataset_revision == 0
            || !valid_key(&required.baseline_id)
            || !evaluations.insert((
                &required.dataset_id,
                required.dataset_revision,
                &required.baseline_id,
            ))
    }) {
        return Err(EvidenceError::InvalidImprovementProposal);
    }
    Ok(())
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4096
}

fn proposal_digest(
    definition: &ImprovementProposal,
    evidence: &[EvidenceReference],
    components: &[String],
    evaluations: &[RequiredEvaluation],
) -> String {
    let mut canonical = format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        definition.id,
        definition.cohort_digest_sha256,
        definition.hypothesis,
        definition.expected_benefit,
        risk_name(definition.risk),
        definition.rollback_plan,
        definition.created_by_principal_digest,
        definition.created_at
    );
    for reference in evidence {
        canonical.push_str(&format!(
            "\ne|{}|{}",
            reference.locator,
            reference.digest_sha256.as_deref().unwrap_or_default()
        ));
    }
    for component in components {
        canonical.push_str(&format!("\nc|{component}"));
    }
    for required in evaluations {
        canonical.push_str(&format!(
            "\nv|{}|{}|{}",
            required.dataset_id, required.dataset_revision, required.baseline_id
        ));
    }
    digest_text(&canonical)
}

fn risk_name(risk: ImprovementRisk) -> &'static str {
    match risk {
        ImprovementRisk::Low => "low",
        ImprovementRisk::Medium => "medium",
        ImprovementRisk::High => "high",
    }
}

pub(super) fn parse_risk(value: &str) -> Result<ImprovementRisk, EvidenceError> {
    match value {
        "low" => Ok(ImprovementRisk::Low),
        "medium" => Ok(ImprovementRisk::Medium),
        "high" => Ok(ImprovementRisk::High),
        _ => Err(EvidenceError::InvalidProposalData),
    }
}

pub(super) fn parse_status(value: &str) -> Result<ImprovementProposalStatus, EvidenceError> {
    match value {
        "draft" => Ok(ImprovementProposalStatus::Draft),
        "ready_for_review" => Ok(ImprovementProposalStatus::ReadyForReview),
        "approved" => Ok(ImprovementProposalStatus::Approved),
        "rejected" => Ok(ImprovementProposalStatus::Rejected),
        "experimenting" => Ok(ImprovementProposalStatus::Experimenting),
        "completed" => Ok(ImprovementProposalStatus::Completed),
        "rolled_back" => Ok(ImprovementProposalStatus::RolledBack),
        _ => Err(EvidenceError::InvalidProposalData),
    }
}
