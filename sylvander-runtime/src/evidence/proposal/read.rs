use rusqlite::OptionalExtension;
use sylvander_protocol::EvidenceReference;

use super::{parse_risk, parse_status};
use crate::evidence::proposal_types::{
    ImprovementProposal, RequiredEvaluation, StoredImprovementProposal,
};
use crate::evidence::{EvidenceError, EvidenceStore};

impl EvidenceStore {
    pub async fn improvement_proposal(
        &self,
        id: String,
    ) -> Result<Option<StoredImprovementProposal>, EvidenceError> {
        self.run(move |connection| {
            let header = connection
                .query_row(
                    "SELECT cohort_digest_sha256, hypothesis, expected_benefit,
                            risk, rollback_plan, created_by_principal_digest,
                            created_at, definition_digest_sha256, status, state_revision
                     FROM evidence_improvement_proposals WHERE id=?1",
                    [&id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, String>(7)?,
                            row.get::<_, String>(8)?,
                            row.get::<_, i64>(9)?,
                        ))
                    },
                )
                .optional()
                .map_err(EvidenceError::sqlite)?;
            let Some(header) = header else {
                return Ok(None);
            };
            Ok(Some(StoredImprovementProposal {
                definition: ImprovementProposal {
                    id: id.clone(),
                    cohort_digest_sha256: header.0,
                    evidence: read_evidence(connection, &id)?,
                    hypothesis: header.1,
                    expected_benefit: header.2,
                    risk: parse_risk(&header.3)?,
                    affected_components: read_components(connection, &id)?,
                    rollback_plan: header.4,
                    required_evaluations: read_evaluations(connection, &id)?,
                    created_by_principal_digest: header.5,
                    created_at: header.6,
                },
                digest_sha256: header.7,
                status: parse_status(&header.8)?,
                state_revision: u64::try_from(header.9)
                    .map_err(|_| EvidenceError::InvalidProposalData)?,
            }))
        })
        .await
    }
}

fn read_evidence(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<Vec<EvidenceReference>, EvidenceError> {
    let mut statement = connection
        .prepare(
            "SELECT locator, digest_sha256
             FROM evidence_improvement_proposal_evidence
             WHERE proposal_id=?1 ORDER BY position",
        )
        .map_err(EvidenceError::sqlite)?;
    let rows = statement
        .query_map([id], |row| {
            Ok(EvidenceReference {
                locator: row.get(0)?,
                digest_sha256: Some(row.get(1)?),
            })
        })
        .map_err(EvidenceError::sqlite)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(EvidenceError::sqlite)
}

fn read_components(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<Vec<String>, EvidenceError> {
    let mut statement = connection
        .prepare(
            "SELECT component FROM evidence_improvement_proposal_components
             WHERE proposal_id=?1 ORDER BY position",
        )
        .map_err(EvidenceError::sqlite)?;
    let rows = statement
        .query_map([id], |row| row.get(0))
        .map_err(EvidenceError::sqlite)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(EvidenceError::sqlite)
}

fn read_evaluations(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<Vec<RequiredEvaluation>, EvidenceError> {
    let mut statement = connection
        .prepare(
            "SELECT dataset_id, dataset_revision, baseline_id
             FROM evidence_improvement_proposal_evaluations
             WHERE proposal_id=?1 ORDER BY position",
        )
        .map_err(EvidenceError::sqlite)?;
    let rows = statement
        .query_map([id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(EvidenceError::sqlite)?;
    rows.map(|row| {
        let row = row.map_err(EvidenceError::sqlite)?;
        Ok(RequiredEvaluation {
            dataset_id: row.0,
            dataset_revision: u64::try_from(row.1)
                .map_err(|_| EvidenceError::InvalidProposalData)?,
            baseline_id: row.2,
        })
    })
    .collect()
}
