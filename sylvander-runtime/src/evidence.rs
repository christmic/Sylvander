//! Durable structured evidence for recovery, audit, and evaluation.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::Mutex;
use tokio::task;

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
    #[error("failed to subscribe evidence recorder: {0}")]
    Subscribe(String),
    #[error("failed to serialize evidence event: {0}")]
    Serialize(String),
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
CREATE INDEX IF NOT EXISTS idx_evidence_events_session ON evidence_events(session_id, sequence);
CREATE INDEX IF NOT EXISTS idx_evidence_turns_session ON evidence_turns(session_id, started_at);
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
}
