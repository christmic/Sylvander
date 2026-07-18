//! Durable structured evidence for recovery, audit, and evaluation.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::Mutex;
use tokio::task;

use sylvander_protocol::{
    EvidenceReference, FeedbackPrivacyClass, FeedbackRating, FeedbackTaskResult, RunFeedback,
};

mod analysis;
mod analysis_types;
mod evaluation;
mod evaluation_types;
mod experiment;
mod experiment_signer;
mod experiment_types;
mod governance;
mod proposal;
mod proposal_types;
mod recorder;

pub use analysis_types::{
    AnalysisPrivacyScope, AnalysisWarning, CohortAnalysis, CohortQuery, CohortTurn,
    FailureBreakdown, FailureClass,
};
pub use evaluation_types::{
    EvaluationBaseline, EvaluationCase, EvaluationComparison, EvaluationDatasetRevision,
    EvaluationSplit, MetricMeasurement, RegressionDecision, RegressionMetric, ScoreDirection,
    ScoringAdapterKind, ScoringAdapterRevision, StoredEvaluationBaseline, StoredEvaluationDataset,
};
pub use experiment_signer::{
    ExperimentEvidenceSigner, HmacSha256EvidenceSigner, sign_experiment_evidence,
    verify_experiment_evidence,
};
pub use experiment_types::{
    ExperimentPhase, ExperimentTransition, RecordExperimentEvidence, SelfChangeExperiment,
    SelfChangeExperimentStatus, SignedExperimentEvidence, StoredSelfChangeExperiment,
    UnsignedExperimentEvidence,
};
pub use governance::{
    EvidenceArtifactSink, EvidenceClassification, EvidenceEncryption, EvidenceExport,
    EvidenceGovernance, EvidenceScope, GovernanceAudit, GovernedRecord, GovernedRecordInput,
    GovernedRecordKind, RetentionSweep, structured_redact,
};
pub use proposal_types::{
    ImprovementProposal, ImprovementProposalStatus, ImprovementRisk, ProposalTransition,
    RequiredEvaluation, StoredImprovementProposal,
};
pub use recorder::EvidenceRecorder;

#[derive(Clone)]
pub struct EvidenceStore {
    connection: Arc<Mutex<Connection>>,
    governance: Option<Arc<governance::GovernanceState>>,
}

#[derive(Debug, Clone)]
pub struct TurnStart {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub started_at: i64,
    pub input_bytes: u64,
    pub input_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StepStart {
    pub id: String,
    pub turn_id: String,
    pub kind: String,
    pub name: String,
    pub started_at: i64,
    pub input_bytes: u64,
    pub input_digest: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EvidenceEvent {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub event_type: String,
    pub occurred_at: i64,
    pub observed_at: i64,
    pub payload_bytes: u64,
    pub payload_digest: Option<String>,
    pub payload_json: Option<String>,
    pub privacy_class: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceCounts {
    pub runs: u64,
    pub turns: u64,
    pub steps: u64,
    pub outcomes: u64,
    pub events: u64,
}

/// A durable, content-free record of a rejected boundary operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationDenial {
    pub id: String,
    pub occurred_at: i64,
    pub request_id: String,
    pub principal_digest: Option<String>,
    pub channel_instance_id: String,
    pub transport: String,
    pub operation: String,
    pub code: String,
    pub resource_digest: Option<String>,
}

/// Content-free audit record for a privileged Agent definition mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAdministrationAudit {
    pub id: String,
    pub occurred_at: i64,
    pub request_id: String,
    pub principal_digest: String,
    pub channel_instance_id: String,
    pub operation: String,
    pub agent_digest: String,
    pub revision: u64,
    pub expected_active_revision: u64,
    pub outcome: String,
    pub error_code: Option<String>,
}

/// Content-free terminal audit for any privileged registry administration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdministrationAudit {
    pub id: String,
    pub occurred_at: i64,
    pub request_id: String,
    pub principal_digest: String,
    pub channel_instance_id: String,
    pub transport: String,
    pub operation: String,
    pub resource_kind: String,
    pub resource_digest: String,
    /// Exact revision/generation when the operation targets one; collection
    /// operations such as `list` carry no fabricated version.
    pub version: Option<u64>,
    pub outcome: String,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TurnQuery {
    pub session_id: Option<String>,
    pub status: Option<String>,
    pub started_after: Option<i64>,
    pub limit: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSummary {
    pub id: String,
    pub run_id: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub status: String,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub step_count: u64,
    pub failed_step_count: u64,
    pub successful_outcome: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// `None` means no priced iteration exists or at least one iteration had
    /// no pricing truth.
    pub cost_nano_usd: Option<u64>,
    pub iteration_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackAttribution {
    pub principal_digest: String,
    pub channel_instance_id: String,
    pub transport: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFeedback {
    pub id: String,
    pub run_id: String,
    pub turn_id: Option<String>,
    pub rating: FeedbackRating,
    pub note: Option<String>,
    pub correction: Option<String>,
    pub tags: Vec<String>,
    pub task_result: Option<FeedbackTaskResult>,
    pub artifacts: Vec<EvidenceReference>,
    pub validations: Vec<EvidenceReference>,
    pub privacy_class: FeedbackPrivacyClass,
    pub attribution: FeedbackAttribution,
    pub recorded_at: i64,
}

impl EvidenceStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, EvidenceError> {
        let path = path.as_ref().to_path_buf();
        Self::open_connection(move || Connection::open(path), None).await
    }

    #[cfg(test)]
    async fn open_in_memory() -> Result<Self, EvidenceError> {
        Self::open_connection(Connection::open_in_memory, None).await
    }

    async fn open_connection(
        open: impl FnOnce() -> rusqlite::Result<Connection> + Send + 'static,
        governance: Option<Arc<governance::GovernanceState>>,
    ) -> Result<Self, EvidenceError> {
        task::spawn_blocking(move || {
            let connection = open().map_err(EvidenceError::sqlite)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(EvidenceError::sqlite)?;
            connection
                .execute_batch(SCHEMA)
                .map_err(EvidenceError::sqlite)?;
            if let Some(state) = governance.as_deref() {
                governance::initialize_governance(&connection, state)?;
            }
            recover_interrupted(&connection)?;
            Ok(Self {
                connection: Arc::new(Mutex::new(connection)),
                governance,
            })
        })
        .await
        .map_err(|error| EvidenceError::Task(error.to_string()))?
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, EvidenceError> + Send + 'static,
    ) -> Result<T, EvidenceError> {
        let connection = self.connection.clone();
        task::spawn_blocking(move || {
            let connection = connection.blocking_lock();
            operation(&connection)
        })
        .await
        .map_err(|error| EvidenceError::Task(error.to_string()))?
    }

    pub async fn start_run(
        &self,
        id: String,
        server_name: String,
        started_at: i64,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            connection
                .execute(
                    "INSERT INTO evidence_runs(id, server_name, started_at, status) VALUES (?1, ?2, ?3, 'running')",
                    params![id, server_name, started_at],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await
    }

    pub async fn finish_run(
        &self,
        id: String,
        ended_at: i64,
        status: &'static str,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            connection
                .execute(
                    "UPDATE evidence_runs SET ended_at=?2, status=?3 WHERE id=?1",
                    params![id, ended_at, status],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await
    }

    pub async fn start_turn(&self, turn: TurnStart) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            connection.execute(
                "INSERT INTO evidence_turns(id, run_id, session_id, agent_id, started_at, status, input_bytes, input_digest) VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7)",
                params![turn.id, turn.run_id, turn.session_id, turn.agent_id, turn.started_at, as_i64(turn.input_bytes)?, turn.input_digest],
            ).map_err(EvidenceError::sqlite)?;
            Ok(())
        }).await
    }

    pub async fn finish_turn(
        &self,
        id: String,
        ended_at: i64,
        status: &'static str,
        output_bytes: u64,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            connection
                .execute(
                    "UPDATE evidence_turns SET ended_at=?2, status=?3, output_bytes=?4 WHERE id=?1",
                    params![id, ended_at, status, as_i64(output_bytes)?],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await
    }

    pub async fn start_step(&self, step: StepStart) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            connection.execute(
                "INSERT OR IGNORE INTO evidence_steps(id, turn_id, kind, name, started_at, status, input_bytes, input_digest) VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7)",
                params![step.id, step.turn_id, step.kind, step.name, step.started_at, as_i64(step.input_bytes)?, step.input_digest],
            ).map_err(EvidenceError::sqlite)?;
            Ok(())
        }).await
    }

    pub async fn finish_step(
        &self,
        id: String,
        ended_at: i64,
        status: &'static str,
        output_bytes: u64,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            connection
                .execute(
                    "UPDATE evidence_steps SET ended_at=?2, status=?3, output_bytes=?4 WHERE id=?1",
                    params![id, ended_at, status, as_i64(output_bytes)?],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await
    }

    pub async fn append_event(&self, event: EvidenceEvent) -> Result<(), EvidenceError> {
        if event.payload_json.is_some() {
            return Err(EvidenceError::PlaintextEvidenceRejected);
        }
        self.run(move |connection| {
            connection.execute(
                "INSERT OR IGNORE INTO evidence_events(id, run_id, session_id, turn_id, event_type, occurred_at, observed_at, payload_bytes, payload_digest, payload_json, privacy_class) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![event.id, event.run_id, event.session_id, event.turn_id, event.event_type, event.occurred_at, event.observed_at, as_i64(event.payload_bytes)?, event.payload_digest, event.payload_json, event.privacy_class],
            ).map_err(EvidenceError::sqlite)?;
            Ok(())
        }).await
    }

    pub async fn record_outcome(
        &self,
        id: String,
        turn_id: String,
        kind: String,
        success: bool,
        recorded_at: i64,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            connection.execute(
                "INSERT OR IGNORE INTO evidence_outcomes(id, turn_id, kind, success, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, turn_id, kind, success, recorded_at],
            ).map_err(EvidenceError::sqlite)?;
            Ok(())
        }).await
    }

    pub async fn counts(&self) -> Result<EvidenceCounts, EvidenceError> {
        self.run(|connection| {
            let count = |table: &str| -> Result<u64, EvidenceError> {
                let value: i64 = connection
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .map_err(EvidenceError::sqlite)?;
                u64::try_from(value).map_err(|_| EvidenceError::InvalidCount(value))
            };
            Ok(EvidenceCounts {
                runs: count("evidence_runs")?,
                turns: count("evidence_turns")?,
                steps: count("evidence_steps")?,
                outcomes: count("evidence_outcomes")?,
                events: count("evidence_events")?,
            })
        })
        .await
    }

    pub async fn record_authorization_denial(
        &self,
        denial: AuthorizationDenial,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            connection
                .execute(
                    "INSERT OR IGNORE INTO authorization_denials(id, occurred_at, request_id, principal_digest, channel_instance_id, transport, operation, code, resource_digest) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![denial.id, denial.occurred_at, denial.request_id, denial.principal_digest, denial.channel_instance_id, denial.transport, denial.operation, denial.code, denial.resource_digest],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await
    }

    pub async fn authorization_denials(
        &self,
        limit: u16,
    ) -> Result<Vec<AuthorizationDenial>, EvidenceError> {
        self.run(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT id, occurred_at, request_id, principal_digest, channel_instance_id, transport, operation, code, resource_digest FROM authorization_denials ORDER BY occurred_at DESC, id DESC LIMIT ?1",
                )
                .map_err(EvidenceError::sqlite)?;
            let rows = statement
                .query_map([i64::from(limit.clamp(1, 1000))], |row| {
                    Ok(AuthorizationDenial {
                        id: row.get(0)?,
                        occurred_at: row.get(1)?,
                        request_id: row.get(2)?,
                        principal_digest: row.get(3)?,
                        channel_instance_id: row.get(4)?,
                        transport: row.get(5)?,
                        operation: row.get(6)?,
                        code: row.get(7)?,
                        resource_digest: row.get(8)?,
                    })
                })
                .map_err(EvidenceError::sqlite)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(EvidenceError::sqlite)
        })
        .await
    }

    pub async fn begin_agent_administration(
        &self,
        audit: AgentAdministrationAudit,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            connection
                .execute(
                    "INSERT INTO agent_administration_audit(id, occurred_at, request_id, principal_digest, channel_instance_id, operation, agent_digest, revision, expected_active_revision, outcome, error_code) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', NULL)",
                    params![audit.id, audit.occurred_at, audit.request_id, audit.principal_digest, audit.channel_instance_id, audit.operation, audit.agent_digest, as_i64(audit.revision)?, as_i64(audit.expected_active_revision)?],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await
    }

    pub async fn finish_agent_administration(
        &self,
        id: String,
        outcome: &'static str,
        error_code: Option<String>,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            let changed = connection
                .execute(
                    "UPDATE agent_administration_audit SET outcome=?2, error_code=?3 WHERE id=?1 AND outcome='pending'",
                    params![id, outcome, error_code],
                )
                .map_err(EvidenceError::sqlite)?;
            if changed != 1 {
                return Err(EvidenceError::InvalidAuditState);
            }
            Ok(())
        })
        .await
    }

    pub async fn agent_administration_audits(
        &self,
        limit: u16,
    ) -> Result<Vec<AgentAdministrationAudit>, EvidenceError> {
        self.run(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT id, occurred_at, request_id, principal_digest, channel_instance_id, operation, agent_digest, revision, expected_active_revision, outcome, error_code FROM agent_administration_audit ORDER BY occurred_at DESC, id DESC LIMIT ?1",
                )
                .map_err(EvidenceError::sqlite)?;
            let rows = statement
                .query_map([i64::from(limit.clamp(1, 1000))], |row| {
                    Ok(AgentAdministrationAudit {
                        id: row.get(0)?,
                        occurred_at: row.get(1)?,
                        request_id: row.get(2)?,
                        principal_digest: row.get(3)?,
                        channel_instance_id: row.get(4)?,
                        operation: row.get(5)?,
                        agent_digest: row.get(6)?,
                        revision: sql_nonnegative(row.get(7)?, 7)?,
                        expected_active_revision: sql_nonnegative(row.get(8)?, 8)?,
                        outcome: row.get(9)?,
                        error_code: row.get(10)?,
                    })
                })
                .map_err(EvidenceError::sqlite)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(EvidenceError::sqlite)
        })
        .await
    }

    /// Persist one already-terminal registry administration decision.
    pub async fn record_administration_audit(
        &self,
        audit: AdministrationAudit,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            let version = audit.version.map(as_i64).transpose()?;
            connection
                .execute(
                    "INSERT INTO administration_audit(id, occurred_at, request_id, principal_digest, channel_instance_id, transport, operation, resource_kind, resource_digest, version, outcome, error_code) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![audit.id, audit.occurred_at, audit.request_id, audit.principal_digest, audit.channel_instance_id, audit.transport, audit.operation, audit.resource_kind, audit.resource_digest, version, audit.outcome, audit.error_code],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await
    }

    /// Persist a mutation intent before any registry state is changed.
    pub async fn begin_administration_mutation(
        &self,
        audit: AdministrationAudit,
    ) -> Result<(), EvidenceError> {
        if audit.outcome != "pending" || audit.error_code.is_some() {
            return Err(EvidenceError::InvalidAuditState);
        }
        self.run(move |connection| {
            let version = audit.version.map(as_i64).transpose()?;
            connection
                .execute(
                    "INSERT INTO administration_audit_intents(id, occurred_at, request_id, principal_digest, channel_instance_id, transport, operation, resource_kind, resource_digest, version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![audit.id, audit.occurred_at, audit.request_id, audit.principal_digest, audit.channel_instance_id, audit.transport, audit.operation, audit.resource_kind, audit.resource_digest, version],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await
    }

    /// Atomically turn one pending mutation intent into a terminal audit.
    pub async fn finish_administration_mutation(
        &self,
        id: String,
        outcome: &'static str,
        error_code: Option<String>,
    ) -> Result<(), EvidenceError> {
        if !matches!(outcome, "succeeded" | "failed") {
            return Err(EvidenceError::InvalidAuditState);
        }
        self.run(move |connection| {
            let transaction = connection
                .unchecked_transaction()
                .map_err(EvidenceError::sqlite)?;
            let inserted = transaction
                .execute(
                    "INSERT INTO administration_audit(id, occurred_at, request_id, principal_digest, channel_instance_id, transport, operation, resource_kind, resource_digest, version, outcome, error_code) SELECT id, occurred_at, request_id, principal_digest, channel_instance_id, transport, operation, resource_kind, resource_digest, version, ?2, ?3 FROM administration_audit_intents WHERE id=?1",
                    params![id, outcome, error_code],
                )
                .map_err(EvidenceError::sqlite)?;
            if inserted != 1 {
                return Err(EvidenceError::InvalidAuditState);
            }
            transaction
                .execute("DELETE FROM administration_audit_intents WHERE id=?1", [&id])
                .map_err(EvidenceError::sqlite)?;
            transaction.commit().map_err(EvidenceError::sqlite)
        })
        .await
    }

    /// Read a bounded newest-first view without registry definitions or secrets.
    pub async fn administration_audits(
        &self,
        limit: u16,
    ) -> Result<Vec<AdministrationAudit>, EvidenceError> {
        self.run(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT id, occurred_at, request_id, principal_digest, channel_instance_id, transport, operation, resource_kind, resource_digest, version, outcome, error_code FROM administration_audit UNION ALL SELECT id, occurred_at, request_id, principal_digest, channel_instance_id, transport, operation, resource_kind, resource_digest, version, 'pending', NULL FROM administration_audit_intents ORDER BY occurred_at DESC, id DESC LIMIT ?1",
                )
                .map_err(EvidenceError::sqlite)?;
            let rows = statement
                .query_map([i64::from(limit.clamp(1, 1000))], |row| {
                    Ok(AdministrationAudit {
                        id: row.get(0)?,
                        occurred_at: row.get(1)?,
                        request_id: row.get(2)?,
                        principal_digest: row.get(3)?,
                        channel_instance_id: row.get(4)?,
                        transport: row.get(5)?,
                        operation: row.get(6)?,
                        resource_kind: row.get(7)?,
                        resource_digest: row.get(8)?,
                        version: row
                            .get::<_, Option<i64>>(9)?
                            .map(|value| sql_nonnegative(value, 9))
                            .transpose()?,
                        outcome: row.get(10)?,
                        error_code: row.get(11)?,
                    })
                })
                .map_err(EvidenceError::sqlite)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(EvidenceError::sqlite)
        })
        .await
    }

    /// Resolve the single session to which feedback may be attributed.
    /// Run-level feedback is accepted only when the run contains one session;
    /// callers must name a turn when a run spans multiple owners.
    pub async fn feedback_session(
        &self,
        run_id: String,
        turn_id: Option<String>,
    ) -> Result<Option<String>, EvidenceError> {
        self.run(move |connection| {
            if let Some(turn_id) = turn_id {
                return connection
                    .query_row(
                        "SELECT session_id FROM evidence_turns WHERE run_id=?1 AND id=?2",
                        params![run_id, turn_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(EvidenceError::sqlite);
            }
            let mut statement = connection
                .prepare("SELECT DISTINCT session_id FROM evidence_turns WHERE run_id=?1 LIMIT 2")
                .map_err(EvidenceError::sqlite)?;
            let sessions = statement
                .query_map([run_id], |row| row.get::<_, String>(0))
                .map_err(EvidenceError::sqlite)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(EvidenceError::sqlite)?;
            Ok((sessions.len() == 1).then(|| sessions[0].clone()))
        })
        .await
    }

    /// Add one model iteration's exact usage to its turn. Missing pricing is
    /// retained as an explicit completeness failure rather than treated as
    /// zero cost.
    pub async fn record_iteration_usage(
        &self,
        turn_id: String,
        input_tokens: u64,
        output_tokens: u64,
        cost_nano_usd: Option<u64>,
    ) -> Result<(), EvidenceError> {
        self.run(move |connection| {
            let changed = connection
                .execute(
                    "UPDATE evidence_turns
                     SET input_tokens=input_tokens+?2,
                         output_tokens=output_tokens+?3,
                         priced_iteration_count=priced_iteration_count+?4,
                         unpriced_iteration_count=unpriced_iteration_count+?5,
                         cost_nano_usd=cost_nano_usd+?6
                     WHERE id=?1",
                    params![
                        turn_id,
                        as_i64(input_tokens)?,
                        as_i64(output_tokens)?,
                        i64::from(cost_nano_usd.is_some()),
                        i64::from(cost_nano_usd.is_none()),
                        as_i64(cost_nano_usd.unwrap_or_default())?,
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            if changed != 1 {
                return Err(EvidenceError::UnknownTurn);
            }
            Ok(())
        })
        .await
    }

    pub async fn turn_usage(&self, turn_id: String) -> Result<Option<TurnUsage>, EvidenceError> {
        self.run(move |connection| {
            connection
                .query_row(
                    "SELECT input_tokens, output_tokens, cost_nano_usd,
                            priced_iteration_count, unpriced_iteration_count
                     FROM evidence_turns WHERE id=?1",
                    [turn_id],
                    |row| {
                        let input_tokens = sql_nonnegative(row.get(0)?, 0)?;
                        let output_tokens = sql_nonnegative(row.get(1)?, 1)?;
                        let cost = sql_nonnegative(row.get(2)?, 2)?;
                        let priced = sql_nonnegative(row.get(3)?, 3)?;
                        let unpriced = sql_nonnegative(row.get(4)?, 4)?;
                        Ok(TurnUsage {
                            input_tokens,
                            output_tokens,
                            cost_nano_usd: (priced > 0 && unpriced == 0).then_some(cost),
                            // Both values originate as non-negative SQLite
                            // i64 values, so their sum fits in u64.
                            iteration_count: priced + unpriced,
                        })
                    },
                )
                .optional()
                .map_err(EvidenceError::sqlite)
        })
        .await
    }

    /// Persist explicit user feedback only when it can be traced to a real
    /// run and, when supplied, a turn belonging to that run.
    pub async fn record_feedback(
        &self,
        feedback: RunFeedback,
        attribution: FeedbackAttribution,
        recorded_at: i64,
    ) -> Result<String, EvidenceError> {
        let id = uuid::Uuid::new_v4().to_string();
        let stored_id = id.clone();
        self.run(move |connection| {
            let target_exists: bool = connection
                .query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM evidence_runs r
                       WHERE r.id=?1
                         AND (?2 IS NULL OR EXISTS(
                           SELECT 1 FROM evidence_turns t
                           WHERE t.id=?2 AND t.run_id=r.id
                         ))
                     )",
                    params![feedback.run_id, feedback.turn_id],
                    |row| row.get(0),
                )
                .map_err(EvidenceError::sqlite)?;
            if !target_exists {
                return Err(EvidenceError::InvalidFeedbackTarget);
            }
            let rating = match feedback.rating {
                FeedbackRating::Positive => "positive",
                FeedbackRating::Negative => "negative",
            };
            let tags_json = serde_json::to_string(&feedback.tags)
                .map_err(|error| EvidenceError::Serialize(error.to_string()))?;
            let artifacts_json = serde_json::to_string(&feedback.artifacts)
                .map_err(|error| EvidenceError::Serialize(error.to_string()))?;
            let validations_json = serde_json::to_string(&feedback.validations)
                .map_err(|error| EvidenceError::Serialize(error.to_string()))?;
            connection
                .execute(
                    "INSERT INTO evidence_feedback(id, run_id, turn_id, rating, note, correction, tags_json, task_result, artifacts_json, validations_json, privacy_class, principal_digest, channel_instance_id, transport, recorded_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                    params![
                        stored_id,
                        feedback.run_id,
                        feedback.turn_id,
                        rating,
                        feedback.note,
                        feedback.correction,
                        tags_json,
                        feedback.task_result.map(task_result_name),
                        artifacts_json,
                        validations_json,
                        privacy_class_name(feedback.privacy_class),
                        attribution.principal_digest,
                        attribution.channel_instance_id,
                        attribution.transport,
                        recorded_at
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await?;
        Ok(id)
    }

    pub async fn feedback(&self, id: String) -> Result<Option<StoredFeedback>, EvidenceError> {
        self.run(move |connection| {
            connection
                .query_row(
                    "SELECT id, run_id, turn_id, rating, note, correction, tags_json, task_result, artifacts_json, validations_json, privacy_class, principal_digest, channel_instance_id, transport, recorded_at FROM evidence_feedback WHERE id=?1",
                    [id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, String>(8)?,
                            row.get::<_, String>(9)?,
                            row.get::<_, String>(10)?,
                            row.get::<_, String>(11)?,
                            row.get::<_, String>(12)?,
                            row.get::<_, String>(13)?,
                            row.get::<_, i64>(14)?,
                        ))
                    },
                )
                .optional()
                .map_err(EvidenceError::sqlite)?
                .map(decode_feedback)
                .transpose()
        })
        .await
    }

    pub async fn feedback_count(&self) -> Result<u64, EvidenceError> {
        self.run(|connection| {
            let value: i64 = connection
                .query_row("SELECT COUNT(*) FROM evidence_feedback", [], |row| {
                    row.get(0)
                })
                .map_err(EvidenceError::sqlite)?;
            u64::try_from(value).map_err(|_| EvidenceError::InvalidCount(value))
        })
        .await
    }

    /// Delete completed runs older than `cutoff` and all normalized evidence
    /// that belongs to them. Active and interrupted recovery records remain.
    pub async fn prune_before(&self, cutoff: i64) -> Result<u64, EvidenceError> {
        self.run(move |connection| {
            let delete = |sql: &str| {
                connection
                    .execute(sql, [cutoff])
                    .map_err(EvidenceError::sqlite)
            };
            delete("DELETE FROM evidence_feedback WHERE run_id IN (SELECT id FROM evidence_runs WHERE ended_at IS NOT NULL AND ended_at < ?1)")?;
            delete("DELETE FROM evidence_events WHERE run_id IN (SELECT id FROM evidence_runs WHERE ended_at IS NOT NULL AND ended_at < ?1)")?;
            delete("DELETE FROM evidence_outcomes WHERE turn_id IN (SELECT id FROM evidence_turns WHERE run_id IN (SELECT id FROM evidence_runs WHERE ended_at IS NOT NULL AND ended_at < ?1))")?;
            delete("DELETE FROM evidence_steps WHERE turn_id IN (SELECT id FROM evidence_turns WHERE run_id IN (SELECT id FROM evidence_runs WHERE ended_at IS NOT NULL AND ended_at < ?1))")?;
            delete("DELETE FROM evidence_turns WHERE run_id IN (SELECT id FROM evidence_runs WHERE ended_at IS NOT NULL AND ended_at < ?1)")?;
            let removed = delete("DELETE FROM evidence_runs WHERE ended_at IS NOT NULL AND ended_at < ?1")?;
            u64::try_from(removed).map_err(|_| EvidenceError::CountTooLarge)
        })
        .await
    }

    /// Inspect one turn's terminal or recovery status.
    pub async fn turn_status(&self, id: String) -> Result<Option<String>, EvidenceError> {
        self.run(move |connection| {
            connection
                .query_row(
                    "SELECT status FROM evidence_turns WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(EvidenceError::sqlite)
        })
        .await
    }

    /// Query bounded turn summaries for recovery dashboards and reproducible
    /// evaluation cohorts. Raw content is deliberately excluded.
    pub async fn query_turns(&self, query: TurnQuery) -> Result<Vec<TurnSummary>, EvidenceError> {
        let limit = i64::from(if query.limit == 0 {
            100
        } else {
            query.limit.min(1000)
        });
        self.run(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT t.id, t.run_id, t.session_id, t.agent_id, t.started_at,
                            t.ended_at, t.status, t.input_bytes, t.output_bytes,
                            COUNT(DISTINCT s.id),
                            COUNT(DISTINCT CASE WHEN s.status='failed' THEN s.id END),
                            MAX(o.success)
                     FROM evidence_turns t
                     LEFT JOIN evidence_steps s ON s.turn_id=t.id
                     LEFT JOIN evidence_outcomes o ON o.turn_id=t.id
                     WHERE (?1 IS NULL OR t.session_id=?1)
                       AND (?2 IS NULL OR t.status=?2)
                       AND (?3 IS NULL OR t.started_at>=?3)
                     GROUP BY t.id
                     ORDER BY t.started_at DESC
                     LIMIT ?4",
                )
                .map_err(EvidenceError::sqlite)?;
            let rows = statement
                .query_map(
                    params![query.session_id, query.status, query.started_after, limit],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, Option<i64>>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, i64>(7)?,
                            row.get::<_, i64>(8)?,
                            row.get::<_, i64>(9)?,
                            row.get::<_, i64>(10)?,
                            row.get::<_, Option<bool>>(11)?,
                        ))
                    },
                )
                .map_err(EvidenceError::sqlite)?;
            rows.map(|row| {
                let row = row.map_err(EvidenceError::sqlite)?;
                Ok(TurnSummary {
                    id: row.0,
                    run_id: row.1,
                    session_id: row.2,
                    agent_id: row.3,
                    started_at: row.4,
                    ended_at: row.5,
                    status: row.6,
                    input_bytes: nonnegative(row.7)?,
                    output_bytes: nonnegative(row.8)?,
                    step_count: nonnegative(row.9)?,
                    failed_step_count: nonnegative(row.10)?,
                    successful_outcome: row.11,
                })
            })
            .collect()
        })
        .await
    }
}

fn recover_interrupted(connection: &Connection) -> Result<(), EvidenceError> {
    connection
        .execute_batch(
            "UPDATE evidence_steps SET status='interrupted' WHERE status='running';
         UPDATE evidence_turns SET status='interrupted' WHERE status='running';
         UPDATE evidence_runs SET status='interrupted' WHERE status='running';",
        )
        .map_err(EvidenceError::sqlite)
}

fn as_i64(value: u64) -> Result<i64, EvidenceError> {
    i64::try_from(value).map_err(|_| EvidenceError::ValueTooLarge(value))
}

fn nonnegative(value: i64) -> Result<u64, EvidenceError> {
    u64::try_from(value).map_err(|_| EvidenceError::InvalidCount(value))
}

fn sql_nonnegative(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn task_result_name(result: FeedbackTaskResult) -> &'static str {
    match result {
        FeedbackTaskResult::Succeeded => "succeeded",
        FeedbackTaskResult::Failed => "failed",
        FeedbackTaskResult::Partial => "partial",
        FeedbackTaskResult::Cancelled => "cancelled",
    }
}

fn privacy_class_name(class: FeedbackPrivacyClass) -> &'static str {
    match class {
        FeedbackPrivacyClass::MetadataOnly => "metadata_only",
        FeedbackPrivacyClass::Private => "private",
        FeedbackPrivacyClass::Shareable => "shareable",
    }
}

type StoredFeedbackRow = (
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
);

fn decode_feedback(row: StoredFeedbackRow) -> Result<StoredFeedback, EvidenceError> {
    let rating = match row.3.as_str() {
        "positive" => FeedbackRating::Positive,
        "negative" => FeedbackRating::Negative,
        _ => return Err(EvidenceError::InvalidFeedbackData),
    };
    let task_result = match row.7.as_deref() {
        None => None,
        Some("succeeded") => Some(FeedbackTaskResult::Succeeded),
        Some("failed") => Some(FeedbackTaskResult::Failed),
        Some("partial") => Some(FeedbackTaskResult::Partial),
        Some("cancelled") => Some(FeedbackTaskResult::Cancelled),
        Some(_) => return Err(EvidenceError::InvalidFeedbackData),
    };
    let privacy_class = match row.10.as_str() {
        "metadata_only" => FeedbackPrivacyClass::MetadataOnly,
        "private" => FeedbackPrivacyClass::Private,
        "shareable" => FeedbackPrivacyClass::Shareable,
        _ => return Err(EvidenceError::InvalidFeedbackData),
    };
    Ok(StoredFeedback {
        id: row.0,
        run_id: row.1,
        turn_id: row.2,
        rating,
        note: row.4,
        correction: row.5,
        tags: serde_json::from_str(&row.6).map_err(|_| EvidenceError::InvalidFeedbackData)?,
        task_result,
        artifacts: serde_json::from_str(&row.8).map_err(|_| EvidenceError::InvalidFeedbackData)?,
        validations: serde_json::from_str(&row.9)
            .map_err(|_| EvidenceError::InvalidFeedbackData)?,
        privacy_class,
        attribution: FeedbackAttribution {
            principal_digest: row.11,
            channel_instance_id: row.12,
            transport: row.13,
        },
        recorded_at: row.14,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("SQLite evidence store failed: {0}")]
    Sqlite(String),
    #[error("evidence blocking task failed: {0}")]
    Task(String),
    #[error("evidence value {0} exceeds SQLite integer range")]
    ValueTooLarge(u64),
    #[error("evidence count is invalid: {0}")]
    InvalidCount(i64),
    #[error("evidence count exceeds supported range")]
    CountTooLarge,
    #[error("failed to subscribe evidence recorder: {0}")]
    Subscribe(String),
    #[error("failed to serialize evidence event: {0}")]
    Serialize(String),
    #[error("feedback must reference an existing run and a turn from that run")]
    InvalidFeedbackTarget,
    #[error("evidence turn does not exist")]
    UnknownTurn,
    #[error("stored feedback is invalid")]
    InvalidFeedbackData,
    #[error("evidence analysis query is invalid")]
    InvalidAnalysisQuery,
    #[error("stored evidence cannot be analyzed safely")]
    InvalidAnalysisData,
    #[error("evaluation registry definition is invalid")]
    InvalidEvaluationDefinition,
    #[error("evaluation registry revision is not the next immutable revision")]
    EvaluationRevisionConflict,
    #[error("stored evaluation registry data is invalid")]
    InvalidEvaluationData,
    #[error("improvement proposal is invalid")]
    InvalidImprovementProposal,
    #[error("improvement proposal state revision or transition is invalid")]
    ProposalStateConflict,
    #[error("stored improvement proposal data is invalid")]
    InvalidProposalData,
    #[error("self-change experiment evidence is invalid")]
    InvalidExperimentEvidence,
    #[error("self-change experiment state revision or transition is invalid")]
    ExperimentStateConflict,
    #[error("stored self-change experiment data is invalid")]
    InvalidExperimentData,
    #[error("Agent administration audit is missing or already terminal")]
    InvalidAuditState,
    #[error("governed evidence requires an encryption key")]
    EncryptionRequired,
    #[error("evidence encryption key must be 32 raw bytes or 64 hexadecimal characters")]
    InvalidEncryptionKey,
    #[error("evidence encryption failed")]
    EncryptionFailed,
    #[error("evidence decryption or authentication failed")]
    DecryptionFailed,
    #[error("evidence database tenant or key binding does not match configuration")]
    GovernanceBindingMismatch,
    #[error("evidence scope does not match the configured tenant")]
    EvidenceScopeMismatch,
    #[error("governed evidence record is invalid")]
    InvalidGovernedRecord,
    #[error("governed evidence record does not exist in this scope")]
    GovernedRecordNotFound,
    #[error("governed evidence record was previously deleted")]
    GovernedRecordDeleted,
    #[error("evidence retention policy is invalid")]
    InvalidRetentionPolicy,
    #[error("plaintext evidence content must use the encrypted governance store")]
    PlaintextEvidenceRejected,
    #[error("evidence governance schema is not the exact current schema")]
    InvalidGovernanceSchema,
}

impl EvidenceError {
    fn sqlite(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

const SCHEMA: &str = r"
PRAGMA foreign_keys=ON;
CREATE TABLE IF NOT EXISTS evidence_runs (
  id TEXT PRIMARY KEY, server_name TEXT NOT NULL, started_at INTEGER NOT NULL,
  ended_at INTEGER, status TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS evidence_turns (
  id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES evidence_runs(id),
  session_id TEXT NOT NULL, agent_id TEXT, started_at INTEGER NOT NULL,
  ended_at INTEGER, status TEXT NOT NULL, input_bytes INTEGER NOT NULL,
  output_bytes INTEGER NOT NULL DEFAULT 0, input_digest TEXT,
  input_tokens INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cost_nano_usd INTEGER NOT NULL DEFAULT 0,
  priced_iteration_count INTEGER NOT NULL DEFAULT 0,
  unpriced_iteration_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS evidence_steps (
  id TEXT PRIMARY KEY, turn_id TEXT NOT NULL REFERENCES evidence_turns(id),
  kind TEXT NOT NULL, name TEXT NOT NULL, started_at INTEGER NOT NULL,
  ended_at INTEGER, status TEXT NOT NULL, input_bytes INTEGER NOT NULL,
  output_bytes INTEGER NOT NULL DEFAULT 0, input_digest TEXT
);
CREATE TABLE IF NOT EXISTS evidence_outcomes (
  id TEXT PRIMARY KEY, turn_id TEXT NOT NULL REFERENCES evidence_turns(id),
  kind TEXT NOT NULL, success INTEGER NOT NULL, recorded_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS evidence_events (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
  run_id TEXT NOT NULL REFERENCES evidence_runs(id), session_id TEXT NOT NULL,
  turn_id TEXT, event_type TEXT NOT NULL, occurred_at INTEGER NOT NULL,
  observed_at INTEGER NOT NULL, payload_bytes INTEGER NOT NULL,
  payload_digest TEXT, payload_json TEXT, privacy_class TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS evidence_feedback (
  id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES evidence_runs(id),
  turn_id TEXT REFERENCES evidence_turns(id), rating TEXT NOT NULL,
  note TEXT, correction TEXT, tags_json TEXT NOT NULL, task_result TEXT,
  artifacts_json TEXT NOT NULL, validations_json TEXT NOT NULL,
  privacy_class TEXT NOT NULL, principal_digest TEXT NOT NULL,
  channel_instance_id TEXT NOT NULL, transport TEXT NOT NULL,
  recorded_at INTEGER NOT NULL,
  CHECK (rating IN ('positive', 'negative')),
  CHECK (task_result IS NULL OR task_result IN ('succeeded', 'failed', 'partial', 'cancelled')),
  CHECK (privacy_class IN ('metadata_only', 'private', 'shareable'))
);
CREATE TABLE IF NOT EXISTS evidence_scoring_adapters (
  id TEXT NOT NULL, revision INTEGER NOT NULL, kind TEXT NOT NULL,
  metric TEXT NOT NULL, config_digest_sha256 TEXT NOT NULL,
  definition_digest_sha256 TEXT NOT NULL, created_at INTEGER NOT NULL,
  PRIMARY KEY(id, revision),
  CHECK (revision > 0),
  CHECK (kind IN ('boolean_validation', 'numeric_metric'))
);
CREATE TABLE IF NOT EXISTS evidence_evaluation_datasets (
  id TEXT NOT NULL, revision INTEGER NOT NULL, name TEXT NOT NULL,
  definition_digest_sha256 TEXT NOT NULL, created_at INTEGER NOT NULL,
  PRIMARY KEY(id, revision),
  CHECK (revision > 0)
);
CREATE TABLE IF NOT EXISTS evidence_evaluation_cases (
  dataset_id TEXT NOT NULL, dataset_revision INTEGER NOT NULL,
  id TEXT NOT NULL, position INTEGER NOT NULL, split TEXT NOT NULL,
  input_locator TEXT NOT NULL, input_digest_sha256 TEXT NOT NULL,
  expected_locator TEXT, expected_digest_sha256 TEXT,
  scorer_id TEXT NOT NULL, scorer_revision INTEGER NOT NULL,
  PRIMARY KEY(dataset_id, dataset_revision, id),
  FOREIGN KEY(dataset_id, dataset_revision)
    REFERENCES evidence_evaluation_datasets(id, revision),
  FOREIGN KEY(scorer_id, scorer_revision)
    REFERENCES evidence_scoring_adapters(id, revision),
  CHECK (position >= 0),
  CHECK (split IN ('fixture', 'held_out')),
  CHECK ((expected_locator IS NULL) = (expected_digest_sha256 IS NULL))
);
CREATE TABLE IF NOT EXISTS evidence_evaluation_baselines (
  id TEXT PRIMARY KEY, dataset_id TEXT NOT NULL, dataset_revision INTEGER NOT NULL,
  definition_digest_sha256 TEXT NOT NULL, recorded_at INTEGER NOT NULL,
  FOREIGN KEY(dataset_id, dataset_revision)
    REFERENCES evidence_evaluation_datasets(id, revision)
);
CREATE TABLE IF NOT EXISTS evidence_evaluation_baseline_metrics (
  baseline_id TEXT NOT NULL REFERENCES evidence_evaluation_baselines(id),
  metric TEXT NOT NULL, direction TEXT NOT NULL, baseline_value INTEGER NOT NULL,
  sample_count INTEGER NOT NULL, max_regression_basis_points INTEGER NOT NULL,
  PRIMARY KEY(baseline_id, metric),
  CHECK (direction IN ('higher_is_better', 'lower_is_better')),
  CHECK (sample_count > 0),
  CHECK (max_regression_basis_points BETWEEN 0 AND 10000)
);
CREATE TABLE IF NOT EXISTS evidence_improvement_proposals (
  id TEXT PRIMARY KEY, cohort_digest_sha256 TEXT NOT NULL,
  hypothesis TEXT NOT NULL, expected_benefit TEXT NOT NULL, risk TEXT NOT NULL,
  rollback_plan TEXT NOT NULL, created_by_principal_digest TEXT NOT NULL,
  created_at INTEGER NOT NULL, definition_digest_sha256 TEXT NOT NULL,
  status TEXT NOT NULL, state_revision INTEGER NOT NULL,
  CHECK (risk IN ('low', 'medium', 'high')),
  CHECK (status IN ('draft', 'ready_for_review', 'approved', 'rejected',
                    'experimenting', 'completed', 'rolled_back')),
  CHECK (state_revision > 0)
);
CREATE TABLE IF NOT EXISTS evidence_improvement_proposal_evidence (
  proposal_id TEXT NOT NULL REFERENCES evidence_improvement_proposals(id),
  position INTEGER NOT NULL, locator TEXT NOT NULL, digest_sha256 TEXT NOT NULL,
  PRIMARY KEY(proposal_id, position)
);
CREATE TABLE IF NOT EXISTS evidence_improvement_proposal_components (
  proposal_id TEXT NOT NULL REFERENCES evidence_improvement_proposals(id),
  position INTEGER NOT NULL, component TEXT NOT NULL,
  PRIMARY KEY(proposal_id, position), UNIQUE(proposal_id, component)
);
CREATE TABLE IF NOT EXISTS evidence_improvement_proposal_evaluations (
  proposal_id TEXT NOT NULL REFERENCES evidence_improvement_proposals(id),
  position INTEGER NOT NULL, dataset_id TEXT NOT NULL,
  dataset_revision INTEGER NOT NULL, baseline_id TEXT NOT NULL,
  PRIMARY KEY(proposal_id, position),
  FOREIGN KEY(dataset_id, dataset_revision)
    REFERENCES evidence_evaluation_datasets(id, revision),
  FOREIGN KEY(baseline_id) REFERENCES evidence_evaluation_baselines(id)
);
CREATE TABLE IF NOT EXISTS evidence_improvement_proposal_transitions (
  proposal_id TEXT NOT NULL REFERENCES evidence_improvement_proposals(id),
  state_revision INTEGER NOT NULL, from_status TEXT NOT NULL,
  to_status TEXT NOT NULL, principal_digest TEXT NOT NULL,
  reason TEXT, occurred_at INTEGER NOT NULL,
  PRIMARY KEY(proposal_id, state_revision)
);
CREATE TABLE IF NOT EXISTS evidence_self_change_experiments (
  id TEXT PRIMARY KEY, proposal_id TEXT NOT NULL UNIQUE
    REFERENCES evidence_improvement_proposals(id),
  lease_id TEXT NOT NULL UNIQUE, branch TEXT NOT NULL, base_commit TEXT NOT NULL,
  proposal_state_revision INTEGER NOT NULL,
  started_by_principal_digest TEXT NOT NULL, created_at INTEGER NOT NULL,
  status TEXT NOT NULL, state_revision INTEGER NOT NULL,
  baseline_bundle_id TEXT, candidate_bundle_id TEXT, merge_commit TEXT,
  rollback_commit TEXT, observation_bundle_id TEXT, merge_approved_by TEXT,
  CHECK (status IN ('prepared', 'candidate_evaluated', 'merge_approved',
                    'merged', 'observing', 'completed', 'rollback_required',
                    'rolled_back', 'failed')),
  CHECK (state_revision > 0)
);
CREATE TABLE IF NOT EXISTS evidence_experiment_bundles (
  id TEXT PRIMARY KEY,
  experiment_id TEXT NOT NULL REFERENCES evidence_self_change_experiments(id),
  phase TEXT NOT NULL, evidence_json TEXT NOT NULL,
  digest_sha256 TEXT NOT NULL, signer_key_id TEXT NOT NULL,
  signature_hex TEXT NOT NULL, recorded_at INTEGER NOT NULL,
  UNIQUE(experiment_id, phase),
  CHECK (phase IN ('baseline', 'candidate', 'observation'))
);
CREATE TABLE IF NOT EXISTS evidence_experiment_transitions (
  experiment_id TEXT NOT NULL REFERENCES evidence_self_change_experiments(id),
  state_revision INTEGER NOT NULL, from_status TEXT NOT NULL,
  to_status TEXT NOT NULL, principal_digest TEXT NOT NULL,
  reason TEXT, occurred_at INTEGER NOT NULL,
  PRIMARY KEY(experiment_id, state_revision)
);
CREATE TABLE IF NOT EXISTS authorization_denials (
  id TEXT PRIMARY KEY, occurred_at INTEGER NOT NULL, request_id TEXT NOT NULL,
  principal_digest TEXT, channel_instance_id TEXT NOT NULL,
  transport TEXT NOT NULL, operation TEXT NOT NULL, code TEXT NOT NULL,
  resource_digest TEXT
);
CREATE TABLE IF NOT EXISTS agent_administration_audit (
  id TEXT PRIMARY KEY, occurred_at INTEGER NOT NULL, request_id TEXT NOT NULL,
  principal_digest TEXT NOT NULL, channel_instance_id TEXT NOT NULL,
  operation TEXT NOT NULL, agent_digest TEXT NOT NULL, revision INTEGER NOT NULL,
  expected_active_revision INTEGER NOT NULL, outcome TEXT NOT NULL,
  error_code TEXT,
  CHECK (outcome IN ('pending', 'succeeded', 'failed'))
);
CREATE TABLE IF NOT EXISTS administration_audit (
  id TEXT PRIMARY KEY, occurred_at INTEGER NOT NULL, request_id TEXT NOT NULL,
  principal_digest TEXT NOT NULL, channel_instance_id TEXT NOT NULL,
  transport TEXT NOT NULL, operation TEXT NOT NULL, resource_kind TEXT NOT NULL,
  resource_digest TEXT NOT NULL, version INTEGER, outcome TEXT NOT NULL,
  error_code TEXT,
  CHECK (version IS NULL OR version > 0),
  CHECK (outcome IN ('succeeded', 'failed', 'denied'))
);
CREATE TABLE IF NOT EXISTS administration_audit_intents (
  id TEXT PRIMARY KEY, occurred_at INTEGER NOT NULL, request_id TEXT NOT NULL,
  principal_digest TEXT NOT NULL, channel_instance_id TEXT NOT NULL,
  transport TEXT NOT NULL, operation TEXT NOT NULL, resource_kind TEXT NOT NULL,
  resource_digest TEXT NOT NULL, version INTEGER,
  CHECK (version IS NULL OR version > 0)
);
CREATE TABLE IF NOT EXISTS evidence_governance_meta (
  singleton INTEGER PRIMARY KEY CHECK(singleton=1),
  tenant_id TEXT NOT NULL, key_id TEXT NOT NULL,
  key_check_nonce BLOB NOT NULL, key_check_ciphertext BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS evidence_governed_records (
  tenant_id TEXT NOT NULL, user_id TEXT NOT NULL, id TEXT NOT NULL,
  kind TEXT NOT NULL, classification TEXT NOT NULL, source_ref TEXT NOT NULL,
  media_type TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  payload_digest_sha256 TEXT NOT NULL, payload_nonce BLOB NOT NULL,
  payload_ciphertext BLOB NOT NULL, key_id TEXT NOT NULL,
  PRIMARY KEY(tenant_id,user_id,id),
  CHECK(kind IN ('event','artifact')),
  CHECK(classification IN ('operational','internal','confidential','restricted')),
  CHECK(expires_at > created_at)
);
CREATE TABLE IF NOT EXISTS evidence_governance_tombstones (
  tenant_id TEXT NOT NULL, user_id TEXT NOT NULL, record_id TEXT NOT NULL,
  kind TEXT NOT NULL, classification TEXT NOT NULL,
  payload_digest_sha256 TEXT NOT NULL, deleted_at INTEGER NOT NULL,
  reason_digest_sha256 TEXT NOT NULL,
  PRIMARY KEY(tenant_id,user_id,record_id),
  CHECK(kind IN ('event','artifact')),
  CHECK(classification IN ('operational','internal','confidential','restricted'))
);
CREATE TABLE IF NOT EXISTS evidence_governance_audit (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE,
  tenant_id TEXT NOT NULL, user_id TEXT NOT NULL, action TEXT NOT NULL,
  selector_digest_sha256 TEXT NOT NULL, result_digest_sha256 TEXT NOT NULL,
  record_count INTEGER NOT NULL, occurred_at INTEGER NOT NULL,
  CHECK(action IN ('export','delete','retention')),
  CHECK(record_count > 0)
);
CREATE INDEX IF NOT EXISTS idx_evidence_events_session ON evidence_events(session_id, sequence);
CREATE INDEX IF NOT EXISTS idx_evidence_turns_session ON evidence_turns(session_id, started_at);
CREATE INDEX IF NOT EXISTS idx_evidence_feedback_run ON evidence_feedback(run_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_authorization_denials_time ON authorization_denials(occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_admin_audit_time ON agent_administration_audit(occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_administration_audit_time ON administration_audit(occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_administration_audit_intents_time ON administration_audit_intents(occurred_at DESC);
CREATE INDEX IF NOT EXISTS idx_governed_records_retention
  ON evidence_governed_records(tenant_id,expires_at,id);
CREATE INDEX IF NOT EXISTS idx_governance_audit_scope
  ON evidence_governance_audit(tenant_id,user_id,sequence DESC);
";

#[cfg(test)]
#[path = "../tests/unit/evidence.rs"]
mod tests;
