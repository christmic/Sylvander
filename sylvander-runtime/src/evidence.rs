//! Durable structured evidence for recovery, audit, and evaluation.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::Mutex;
use tokio::task;

use sylvander_protocol::{FeedbackRating, RunFeedback};

mod recorder;

pub use recorder::EvidenceRecorder;

#[derive(Clone)]
pub struct EvidenceStore {
    connection: Arc<Mutex<Connection>>,
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

impl EvidenceStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, EvidenceError> {
        let path = path.as_ref().to_path_buf();
        Self::open_connection(move || Connection::open(path)).await
    }

    #[cfg(test)]
    async fn open_in_memory() -> Result<Self, EvidenceError> {
        Self::open_connection(Connection::open_in_memory).await
    }

    async fn open_connection(
        open: impl FnOnce() -> rusqlite::Result<Connection> + Send + 'static,
    ) -> Result<Self, EvidenceError> {
        task::spawn_blocking(move || {
            let connection = open().map_err(EvidenceError::sqlite)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(EvidenceError::sqlite)?;
            connection
                .execute_batch(SCHEMA)
                .map_err(EvidenceError::sqlite)?;
            recover_interrupted(&connection)?;
            Ok(Self {
                connection: Arc::new(Mutex::new(connection)),
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

    /// Persist explicit user feedback only when it can be traced to a real
    /// run and, when supplied, a turn belonging to that run.
    pub async fn record_feedback(
        &self,
        feedback: RunFeedback,
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
            connection
                .execute(
                    "INSERT INTO evidence_feedback(id, run_id, turn_id, rating, note, tags_json, recorded_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        stored_id,
                        feedback.run_id,
                        feedback.turn_id,
                        rating,
                        feedback.note,
                        tags_json,
                        recorded_at
                    ],
                )
                .map_err(EvidenceError::sqlite)?;
            Ok(())
        })
        .await?;
        Ok(id)
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
  output_bytes INTEGER NOT NULL DEFAULT 0, input_digest TEXT
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
  note TEXT, tags_json TEXT NOT NULL, recorded_at INTEGER NOT NULL,
  CHECK (rating IN ('positive', 'negative'))
);
CREATE TABLE IF NOT EXISTS authorization_denials (
  id TEXT PRIMARY KEY, occurred_at INTEGER NOT NULL, request_id TEXT NOT NULL,
  principal_digest TEXT, channel_instance_id TEXT NOT NULL,
  transport TEXT NOT NULL, operation TEXT NOT NULL, code TEXT NOT NULL,
  resource_digest TEXT
);
CREATE INDEX IF NOT EXISTS idx_evidence_events_session ON evidence_events(session_id, sequence);
CREATE INDEX IF NOT EXISTS idx_evidence_turns_session ON evidence_turns(session_id, started_at);
CREATE INDEX IF NOT EXISTS idx_evidence_feedback_run ON evidence_feedback(run_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_authorization_denials_time ON authorization_denials(occurred_at DESC);
";

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stores_structured_run_turn_step_outcome_and_event() {
        let store = EvidenceStore::open_in_memory().await.unwrap();
        store
            .start_run("run-1".into(), "test".into(), 1)
            .await
            .unwrap();
        store
            .start_turn(TurnStart {
                id: "turn-1".into(),
                run_id: "run-1".into(),
                session_id: "session-1".into(),
                agent_id: Some("agent-1".into()),
                started_at: 2,
                input_bytes: 5,
                input_digest: Some("digest".into()),
            })
            .await
            .unwrap();
        store
            .start_step(StepStart {
                id: "tool-1".into(),
                turn_id: "turn-1".into(),
                kind: "tool".into(),
                name: "read".into(),
                started_at: 3,
                input_bytes: 2,
                input_digest: None,
            })
            .await
            .unwrap();
        store
            .finish_step("tool-1".into(), 4, "succeeded", 7)
            .await
            .unwrap();
        store
            .record_outcome(
                "outcome-1".into(),
                "turn-1".into(),
                "completed".into(),
                true,
                5,
            )
            .await
            .unwrap();
        store
            .append_event(EvidenceEvent {
                id: "event-1".into(),
                run_id: "run-1".into(),
                session_id: "session-1".into(),
                turn_id: Some("turn-1".into()),
                event_type: "done".into(),
                occurred_at: 5,
                observed_at: 5,
                payload_bytes: 7,
                payload_digest: None,
                payload_json: None,
                privacy_class: "user_content".into(),
            })
            .await
            .unwrap();
        store
            .finish_turn("turn-1".into(), 5, "succeeded", 7)
            .await
            .unwrap();
        store
            .finish_run("run-1".into(), 6, "completed")
            .await
            .unwrap();
        assert_eq!(
            store.counts().await.unwrap(),
            EvidenceCounts {
                runs: 1,
                turns: 1,
                steps: 1,
                outcomes: 1,
                events: 1
            }
        );
        let turns = store
            .query_turns(TurnQuery {
                session_id: Some("session-1".into()),
                status: Some("succeeded".into()),
                started_after: Some(1),
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].step_count, 1);
        assert_eq!(turns[0].failed_step_count, 0);
        assert_eq!(turns[0].successful_outcome, Some(true));
    }

    #[tokio::test]
    async fn authorization_denials_are_durable_and_content_free() {
        let store = EvidenceStore::open_in_memory().await.unwrap();
        let denial = AuthorizationDenial {
            id: "denial-1".into(),
            occurred_at: 42,
            request_id: "request-1".into(),
            principal_digest: Some("principal-digest".into()),
            channel_instance_id: "desktop-primary".into(),
            transport: "websocket".into(),
            operation: "load_session".into(),
            code: "forbidden".into(),
            resource_digest: Some("resource-digest".into()),
        };
        store
            .record_authorization_denial(denial.clone())
            .await
            .unwrap();
        assert_eq!(store.authorization_denials(10).await.unwrap(), vec![denial]);
    }

    #[tokio::test]
    async fn feedback_requires_traceable_run_and_turn_evidence() {
        let store = EvidenceStore::open_in_memory().await.unwrap();
        store
            .start_run("run-1".into(), "test".into(), 1)
            .await
            .unwrap();
        store
            .start_turn(TurnStart {
                id: "turn-1".into(),
                run_id: "run-1".into(),
                session_id: "session-1".into(),
                agent_id: Some("agent-1".into()),
                started_at: 2,
                input_bytes: 0,
                input_digest: None,
            })
            .await
            .unwrap();

        let feedback_id = store
            .record_feedback(
                RunFeedback {
                    run_id: "run-1".into(),
                    turn_id: Some("turn-1".into()),
                    rating: FeedbackRating::Positive,
                    note: Some("useful".into()),
                    tags: vec!["correct".into()],
                },
                3,
            )
            .await
            .unwrap();
        assert!(!feedback_id.is_empty());
        assert_eq!(store.feedback_count().await.unwrap(), 1);

        let error = store
            .record_feedback(
                RunFeedback {
                    run_id: "run-1".into(),
                    turn_id: Some("unknown-turn".into()),
                    rating: FeedbackRating::Negative,
                    note: None,
                    tags: Vec::new(),
                },
                4,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, EvidenceError::InvalidFeedbackTarget));
        assert_eq!(store.feedback_count().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn reopening_marks_inflight_records_interrupted() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = directory.path().join("evidence.db");
        let store = EvidenceStore::open(&path).await.unwrap();
        store
            .start_run("run-1".into(), "test".into(), 1)
            .await
            .unwrap();
        store
            .start_turn(TurnStart {
                id: "turn-1".into(),
                run_id: "run-1".into(),
                session_id: "session-1".into(),
                agent_id: None,
                started_at: 2,
                input_bytes: 0,
                input_digest: None,
            })
            .await
            .unwrap();
        drop(store);

        let reopened = EvidenceStore::open(path).await.unwrap();
        assert_eq!(
            reopened.turn_status("turn-1".into()).await.unwrap(),
            Some("interrupted".into())
        );
    }

    #[tokio::test]
    async fn retention_removes_only_completed_old_runs() {
        let store = EvidenceStore::open_in_memory().await.unwrap();
        store
            .start_run("old".into(), "test".into(), 1)
            .await
            .unwrap();
        store
            .finish_run("old".into(), 2, "completed")
            .await
            .unwrap();
        store
            .start_run("active".into(), "test".into(), 1)
            .await
            .unwrap();

        assert_eq!(store.prune_before(3).await.unwrap(), 1);
        assert_eq!(store.counts().await.unwrap().runs, 1);
    }
}
