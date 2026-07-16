use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};

use super::{digest_text, valid_key};
use crate::evidence::evaluation_types::{
    EvaluationBaseline, EvaluationComparison, MetricMeasurement, RegressionDecision,
    RegressionMetric, ScoreDirection, StoredEvaluationBaseline,
};
use crate::evidence::{EvidenceError, EvidenceStore, as_i64};

impl EvidenceStore {
    /// Persist one immutable baseline and its explicit regression thresholds.
    pub async fn register_evaluation_baseline(
        &self,
        definition: EvaluationBaseline,
    ) -> Result<String, EvidenceError> {
        validate_baseline(&definition)?;
        let mut metrics = definition.metrics.clone();
        metrics.sort_by(|left, right| left.metric.cmp(&right.metric));
        let digest = baseline_digest(&definition, &metrics);
        let stored_digest = digest.clone();
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let existing = transaction
                .query_row(
                    "SELECT definition_digest_sha256
                     FROM evidence_evaluation_baselines WHERE id=?1",
                    [&definition.id],
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
            let dataset_exists = transaction
                .query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM evidence_evaluation_datasets
                       WHERE id=?1 AND revision=?2
                     )",
                    params![definition.dataset_id, as_i64(definition.dataset_revision)?],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(EvidenceError::sqlite)?;
            if !dataset_exists {
                return Err(EvidenceError::InvalidEvaluationDefinition);
            }
            for metric in &metrics {
                let metric_exists = transaction
                    .query_row(
                        "SELECT EXISTS(
                           SELECT 1
                           FROM evidence_evaluation_cases c
                           JOIN evidence_scoring_adapters s
                             ON s.id=c.scorer_id AND s.revision=c.scorer_revision
                           WHERE c.dataset_id=?1 AND c.dataset_revision=?2
                             AND s.metric=?3
                         )",
                        params![
                            definition.dataset_id,
                            as_i64(definition.dataset_revision)?,
                            metric.metric
                        ],
                        |row| row.get::<_, bool>(0),
                    )
                    .map_err(EvidenceError::sqlite)?;
                if !metric_exists {
                    return Err(EvidenceError::InvalidEvaluationDefinition);
                }
            }
            transaction
                .execute(
                    "INSERT INTO evidence_evaluation_baselines(
                       id, dataset_id, dataset_revision,
                       definition_digest_sha256, recorded_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        definition.id,
                        definition.dataset_id,
                        as_i64(definition.dataset_revision)?,
                        stored_digest,
                        definition.recorded_at
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            for metric in metrics {
                transaction
                    .execute(
                        "INSERT INTO evidence_evaluation_baseline_metrics(
                           baseline_id, metric, direction, baseline_value,
                           sample_count, max_regression_basis_points
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            definition.id,
                            metric.metric,
                            direction_name(metric.direction),
                            metric.baseline_value,
                            as_i64(metric.sample_count)?,
                            i64::from(metric.max_regression_basis_points)
                        ],
                    )
                    .map_err(EvidenceError::sqlite)?;
            }
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await?;
        Ok(digest)
    }

    pub async fn evaluation_baseline(
        &self,
        id: String,
    ) -> Result<Option<StoredEvaluationBaseline>, EvidenceError> {
        self.run(move |connection| {
            let header = connection
                .query_row(
                    "SELECT dataset_id, dataset_revision,
                            definition_digest_sha256, recorded_at
                     FROM evidence_evaluation_baselines WHERE id=?1",
                    [&id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .optional()
                .map_err(EvidenceError::sqlite)?;
            let Some((dataset_id, dataset_revision, digest_sha256, recorded_at)) = header else {
                return Ok(None);
            };
            let mut statement = connection
                .prepare(
                    "SELECT metric, direction, baseline_value,
                            sample_count, max_regression_basis_points
                     FROM evidence_evaluation_baseline_metrics
                     WHERE baseline_id=?1 ORDER BY metric ASC",
                )
                .map_err(EvidenceError::sqlite)?;
            let rows = statement
                .query_map([&id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })
                .map_err(EvidenceError::sqlite)?;
            let metrics = rows
                .map(|row| {
                    let row = row.map_err(EvidenceError::sqlite)?;
                    Ok(RegressionMetric {
                        metric: row.0,
                        direction: parse_direction(&row.1)?,
                        baseline_value: row.2,
                        sample_count: u64::try_from(row.3)
                            .map_err(|_| EvidenceError::InvalidEvaluationData)?,
                        max_regression_basis_points: u16::try_from(row.4)
                            .map_err(|_| EvidenceError::InvalidEvaluationData)?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Some(StoredEvaluationBaseline {
                definition: EvaluationBaseline {
                    id,
                    dataset_id,
                    dataset_revision: u64::try_from(dataset_revision)
                        .map_err(|_| EvidenceError::InvalidEvaluationData)?,
                    metrics,
                    recorded_at,
                },
                digest_sha256,
            }))
        })
        .await
    }

    /// Compare one complete candidate measurement set against an immutable
    /// baseline. Missing, extra, duplicate, or differently sampled metrics
    /// fail instead of producing a partial pass.
    pub async fn compare_evaluation_baseline(
        &self,
        baseline_id: String,
        mut measurements: Vec<MetricMeasurement>,
    ) -> Result<EvaluationComparison, EvidenceError> {
        if measurements.is_empty() || measurements.len() > 128 {
            return Err(EvidenceError::InvalidEvaluationDefinition);
        }
        measurements.sort_by(|left, right| left.metric.cmp(&right.metric));
        if measurements
            .windows(2)
            .any(|pair| pair[0].metric == pair[1].metric)
        {
            return Err(EvidenceError::InvalidEvaluationDefinition);
        }
        let baseline = self
            .evaluation_baseline(baseline_id.clone())
            .await?
            .ok_or(EvidenceError::InvalidEvaluationDefinition)?;
        if baseline.definition.metrics.len() != measurements.len() {
            return Err(EvidenceError::InvalidEvaluationDefinition);
        }
        let mut decisions = Vec::with_capacity(measurements.len());
        for (metric, measurement) in baseline.definition.metrics.iter().zip(measurements) {
            if metric.metric != measurement.metric
                || metric.sample_count != measurement.sample_count
                || !valid_key(&measurement.metric)
            {
                return Err(EvidenceError::InvalidEvaluationDefinition);
            }
            let allowed_boundary = regression_boundary(metric)?;
            let passed = match metric.direction {
                ScoreDirection::HigherIsBetter => measurement.value >= allowed_boundary,
                ScoreDirection::LowerIsBetter => measurement.value <= allowed_boundary,
            };
            decisions.push(RegressionDecision {
                metric: metric.metric.clone(),
                direction: metric.direction,
                baseline_value: metric.baseline_value,
                candidate_value: measurement.value,
                allowed_boundary,
                sample_count: measurement.sample_count,
                passed,
            });
        }
        Ok(EvaluationComparison {
            baseline_id,
            baseline_digest_sha256: baseline.digest_sha256,
            passed: decisions.iter().all(|decision| decision.passed),
            decisions,
        })
    }
}

fn validate_baseline(definition: &EvaluationBaseline) -> Result<(), EvidenceError> {
    if !valid_key(&definition.id)
        || !valid_key(&definition.dataset_id)
        || definition.dataset_revision == 0
        || definition.metrics.is_empty()
        || definition.metrics.len() > 128
        || definition.recorded_at < 0
    {
        return Err(EvidenceError::InvalidEvaluationDefinition);
    }
    let mut names = BTreeSet::new();
    for metric in &definition.metrics {
        if !valid_key(&metric.metric)
            || !names.insert(&metric.metric)
            || metric.sample_count == 0
            || metric.max_regression_basis_points > 10_000
        {
            return Err(EvidenceError::InvalidEvaluationDefinition);
        }
    }
    Ok(())
}

fn baseline_digest(definition: &EvaluationBaseline, metrics: &[RegressionMetric]) -> String {
    let mut canonical = format!(
        "{}|{}|{}|{}",
        definition.id, definition.dataset_id, definition.dataset_revision, definition.recorded_at
    );
    for metric in metrics {
        canonical.push_str(&format!(
            "\n{}|{}|{}|{}|{}",
            metric.metric,
            direction_name(metric.direction),
            metric.baseline_value,
            metric.sample_count,
            metric.max_regression_basis_points
        ));
    }
    digest_text(&canonical)
}

fn direction_name(direction: ScoreDirection) -> &'static str {
    match direction {
        ScoreDirection::HigherIsBetter => "higher_is_better",
        ScoreDirection::LowerIsBetter => "lower_is_better",
    }
}

fn parse_direction(value: &str) -> Result<ScoreDirection, EvidenceError> {
    match value {
        "higher_is_better" => Ok(ScoreDirection::HigherIsBetter),
        "lower_is_better" => Ok(ScoreDirection::LowerIsBetter),
        _ => Err(EvidenceError::InvalidEvaluationData),
    }
}

fn regression_boundary(metric: &RegressionMetric) -> Result<i64, EvidenceError> {
    let baseline = i128::from(metric.baseline_value);
    let delta =
        baseline.abs() * i128::from(metric.max_regression_basis_points) / i128::from(10_000);
    let boundary = match metric.direction {
        ScoreDirection::HigherIsBetter => baseline - delta,
        ScoreDirection::LowerIsBetter => baseline + delta,
    };
    i64::try_from(boundary).map_err(|_| EvidenceError::InvalidEvaluationDefinition)
}
