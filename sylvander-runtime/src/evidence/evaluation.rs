use std::fmt::Write;

use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::evaluation_types::{ScoringAdapterKind, ScoringAdapterRevision};
use super::{EvidenceError, EvidenceStore, as_i64};

impl EvidenceStore {
    /// Register an immutable scoring adapter revision. Repeating the exact
    /// revision is idempotent; changed content or skipped revisions fail.
    pub async fn register_scoring_adapter(
        &self,
        definition: ScoringAdapterRevision,
    ) -> Result<String, EvidenceError> {
        validate_scorer(&definition)?;
        let digest = scorer_digest(&definition);
        let stored_digest = digest.clone();
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let existing = transaction
                .query_row(
                    "SELECT definition_digest_sha256
                     FROM evidence_scoring_adapters WHERE id=?1 AND revision=?2",
                    params![definition.id, as_i64(definition.revision)?],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(EvidenceError::sqlite)?;
            if let Some(existing) = existing {
                return if existing == stored_digest {
                    Ok(())
                } else {
                    Err(EvidenceError::EvaluationRevisionConflict)
                };
            }
            let current = transaction
                .query_row(
                    "SELECT COALESCE(MAX(revision), 0)
                     FROM evidence_scoring_adapters WHERE id=?1",
                    [&definition.id],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(EvidenceError::sqlite)?;
            let expected = u64::try_from(current)
                .map_err(|_| EvidenceError::InvalidEvaluationData)?
                .checked_add(1)
                .ok_or(EvidenceError::CountTooLarge)?;
            if definition.revision != expected {
                return Err(EvidenceError::EvaluationRevisionConflict);
            }
            transaction
                .execute(
                    "INSERT INTO evidence_scoring_adapters(
                       id, revision, kind, metric, config_digest_sha256,
                       definition_digest_sha256, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        definition.id,
                        as_i64(definition.revision)?,
                        scorer_kind_name(definition.kind),
                        definition.metric,
                        definition.config_digest_sha256,
                        stored_digest,
                        definition.created_at,
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await?;
        Ok(digest)
    }

    pub async fn scoring_adapter(
        &self,
        id: String,
        revision: u64,
    ) -> Result<Option<ScoringAdapterRevision>, EvidenceError> {
        self.run(move |connection| {
            connection
                .query_row(
                    "SELECT id, revision, kind, metric, config_digest_sha256, created_at
                     FROM evidence_scoring_adapters WHERE id=?1 AND revision=?2",
                    params![id, as_i64(revision)?],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, i64>(5)?,
                        ))
                    },
                )
                .optional()
                .map_err(EvidenceError::sqlite)?
                .map(|row| {
                    Ok(ScoringAdapterRevision {
                        id: row.0,
                        revision: u64::try_from(row.1)
                            .map_err(|_| EvidenceError::InvalidEvaluationData)?,
                        kind: parse_scorer_kind(&row.2)?,
                        metric: row.3,
                        config_digest_sha256: row.4,
                        created_at: row.5,
                    })
                })
                .transpose()
        })
        .await
    }
}

pub(super) fn valid_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(super) fn digest_text(value: &str) -> String {
    let bytes = Sha256::digest(value.as_bytes());
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn validate_scorer(definition: &ScoringAdapterRevision) -> Result<(), EvidenceError> {
    if !valid_key(&definition.id)
        || definition.revision == 0
        || !valid_key(&definition.metric)
        || !valid_sha256(&definition.config_digest_sha256)
        || definition.created_at < 0
    {
        return Err(EvidenceError::InvalidEvaluationDefinition);
    }
    Ok(())
}

fn scorer_digest(definition: &ScoringAdapterRevision) -> String {
    digest_text(&format!(
        "{}|{}|{}|{}|{}|{}",
        definition.id,
        definition.revision,
        scorer_kind_name(definition.kind),
        definition.metric,
        definition.config_digest_sha256,
        definition.created_at
    ))
}

pub(super) fn scorer_kind_name(kind: ScoringAdapterKind) -> &'static str {
    match kind {
        ScoringAdapterKind::BooleanValidation => "boolean_validation",
        ScoringAdapterKind::NumericMetric => "numeric_metric",
    }
}

fn parse_scorer_kind(value: &str) -> Result<ScoringAdapterKind, EvidenceError> {
    match value {
        "boolean_validation" => Ok(ScoringAdapterKind::BooleanValidation),
        "numeric_metric" => Ok(ScoringAdapterKind::NumericMetric),
        _ => Err(EvidenceError::InvalidEvaluationData),
    }
}

#[cfg(test)]
mod tests;
