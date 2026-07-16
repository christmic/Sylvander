use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};
use sylvander_protocol::EvidenceReference;

use super::{digest_text, valid_key, valid_sha256};
use crate::evidence::evaluation_types::{
    EvaluationCase, EvaluationDatasetRevision, EvaluationSplit, StoredEvaluationDataset,
};
use crate::evidence::{EvidenceError, EvidenceStore, as_i64};

impl EvidenceStore {
    /// Register an immutable dataset revision with deterministically ordered
    /// fixture and held-out cases.
    pub async fn register_evaluation_dataset(
        &self,
        definition: EvaluationDatasetRevision,
    ) -> Result<String, EvidenceError> {
        validate_dataset(&definition)?;
        let mut cases = definition.cases.clone();
        cases.sort_by(|left, right| left.id.cmp(&right.id));
        let digest = dataset_digest(&definition, &cases);
        let stored_digest = digest.clone();
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let existing = transaction
                .query_row(
                    "SELECT definition_digest_sha256
                     FROM evidence_evaluation_datasets
                     WHERE id=?1 AND revision=?2",
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
                     FROM evidence_evaluation_datasets WHERE id=?1",
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
            for case in &cases {
                let scorer_exists = transaction
                    .query_row(
                        "SELECT EXISTS(
                           SELECT 1 FROM evidence_scoring_adapters
                           WHERE id=?1 AND revision=?2
                         )",
                        params![case.scorer_id, as_i64(case.scorer_revision)?],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(EvidenceError::sqlite)?;
                if !scorer_exists {
                    return Err(EvidenceError::InvalidEvaluationDefinition);
                }
            }
            transaction
                .execute(
                    "INSERT INTO evidence_evaluation_datasets(
                       id, revision, name, definition_digest_sha256, created_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        definition.id,
                        as_i64(definition.revision)?,
                        definition.name,
                        stored_digest,
                        definition.created_at,
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            for (position, case) in cases.into_iter().enumerate() {
                let (expected_locator, expected_digest) =
                    case.expected.map_or((None, None), |reference| {
                        (Some(reference.locator), reference.digest_sha256)
                    });
                transaction
                    .execute(
                        "INSERT INTO evidence_evaluation_cases(
                           dataset_id, dataset_revision, id, position, split,
                           input_locator, input_digest_sha256,
                           expected_locator, expected_digest_sha256,
                           scorer_id, scorer_revision
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                        params![
                            definition.id,
                            as_i64(definition.revision)?,
                            case.id,
                            as_i64(position as u64)?,
                            split_name(case.split),
                            case.input.locator,
                            case.input.digest_sha256,
                            expected_locator,
                            expected_digest,
                            case.scorer_id,
                            as_i64(case.scorer_revision)?,
                        ],
                    )
                    .map_err(EvidenceError::sqlite)?;
            }
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await?;
        Ok(digest)
    }

    pub async fn evaluation_dataset(
        &self,
        id: String,
        revision: u64,
    ) -> Result<Option<StoredEvaluationDataset>, EvidenceError> {
        self.run(move |connection| {
            let header = connection
                .query_row(
                    "SELECT name, definition_digest_sha256, created_at
                     FROM evidence_evaluation_datasets
                     WHERE id=?1 AND revision=?2",
                    params![id, as_i64(revision)?],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(EvidenceError::sqlite)?;
            let Some((name, digest_sha256, created_at)) = header else {
                return Ok(None);
            };
            let mut statement = connection
                .prepare(
                    "SELECT id, split, input_locator, input_digest_sha256,
                            expected_locator, expected_digest_sha256,
                            scorer_id, scorer_revision
                     FROM evidence_evaluation_cases
                     WHERE dataset_id=?1 AND dataset_revision=?2
                     ORDER BY position ASC",
                )
                .map_err(EvidenceError::sqlite)?;
            let rows = statement
                .query_map(params![id, as_i64(revision)?], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                })
                .map_err(EvidenceError::sqlite)?;
            let cases = rows
                .map(|row| {
                    let row = row.map_err(EvidenceError::sqlite)?;
                    let expected = match (row.4, row.5) {
                        (Some(locator), Some(digest)) => Some(EvidenceReference {
                            locator,
                            digest_sha256: Some(digest),
                        }),
                        (None, None) => None,
                        _ => return Err(EvidenceError::InvalidEvaluationData),
                    };
                    Ok(EvaluationCase {
                        id: row.0,
                        split: parse_split(&row.1)?,
                        input: EvidenceReference {
                            locator: row.2,
                            digest_sha256: Some(row.3),
                        },
                        expected,
                        scorer_id: row.6,
                        scorer_revision: u64::try_from(row.7)
                            .map_err(|_| EvidenceError::InvalidEvaluationData)?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(StoredEvaluationDataset {
                definition: EvaluationDatasetRevision {
                    id,
                    revision,
                    name,
                    cases,
                    created_at,
                },
                digest_sha256,
            }))
        })
        .await
    }
}

fn validate_dataset(definition: &EvaluationDatasetRevision) -> Result<(), EvidenceError> {
    if !valid_key(&definition.id)
        || definition.revision == 0
        || definition.name.trim().is_empty()
        || definition.name.len() > 256
        || !(2..=10_000).contains(&definition.cases.len())
        || definition.created_at < 0
    {
        return Err(EvidenceError::InvalidEvaluationDefinition);
    }
    let mut ids = BTreeSet::new();
    let mut fixture = false;
    let mut held_out = false;
    for case in &definition.cases {
        fixture |= case.split == EvaluationSplit::Fixture;
        held_out |= case.split == EvaluationSplit::HeldOut;
        if !valid_key(&case.id)
            || !ids.insert(&case.id)
            || !valid_reference(&case.input)
            || case
                .expected
                .as_ref()
                .is_some_and(|value| !valid_reference(value))
            || !valid_key(&case.scorer_id)
            || case.scorer_revision == 0
        {
            return Err(EvidenceError::InvalidEvaluationDefinition);
        }
    }
    if !fixture || !held_out {
        return Err(EvidenceError::InvalidEvaluationDefinition);
    }
    Ok(())
}

fn valid_reference(reference: &EvidenceReference) -> bool {
    !reference.locator.is_empty()
        && reference.locator.len() <= 1024
        && reference.digest_sha256.as_deref().is_some_and(valid_sha256)
}

fn dataset_digest(
    definition: &EvaluationDatasetRevision,
    ordered_cases: &[EvaluationCase],
) -> String {
    let mut canonical = format!(
        "{}|{}|{}|{}",
        definition.id, definition.revision, definition.name, definition.created_at
    );
    for case in ordered_cases {
        canonical.push_str(&format!(
            "\n{}|{}|{}|{}|{}|{}|{}|{}",
            case.id,
            split_name(case.split),
            case.input.locator,
            case.input.digest_sha256.as_deref().unwrap_or_default(),
            case.expected
                .as_ref()
                .map_or("", |reference| reference.locator.as_str()),
            case.expected
                .as_ref()
                .and_then(|reference| reference.digest_sha256.as_deref())
                .unwrap_or_default(),
            case.scorer_id,
            case.scorer_revision
        ));
    }
    digest_text(&canonical)
}

fn split_name(split: EvaluationSplit) -> &'static str {
    match split {
        EvaluationSplit::Fixture => "fixture",
        EvaluationSplit::HeldOut => "held_out",
    }
}

fn parse_split(value: &str) -> Result<EvaluationSplit, EvidenceError> {
    match value {
        "fixture" => Ok(EvaluationSplit::Fixture),
        "held_out" => Ok(EvaluationSplit::HeldOut),
        _ => Err(EvidenceError::InvalidEvaluationData),
    }
}
