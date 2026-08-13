use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::{RuntimeBenchCoordinate, RuntimeBenchPlan, RuntimeBenchResult};

const APPLICATION_ID: i64 = 0x5359_4252;
const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Inserted,
    AlreadyPresent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanCoverage {
    pub expected: u64,
    pub recorded: u64,
    pub missing: u64,
    pub unexpected: u64,
}

impl PlanCoverage {
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.missing == 0 && self.unexpected == 0 && self.expected == self.recorded
    }
}

/// Independent `SQLite` evidence ledger for benchmark runs.
///
/// This store deliberately lives outside Runtime's operational session
/// database. A coordinate is write-once: an exact retry is idempotent while a
/// different result for the same coordinate is rejected.
pub struct BenchmarkLedger {
    connection: Connection,
}

impl BenchmarkLedger {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BenchmarkLedgerError> {
        let connection = Connection::open(path).map_err(|_| BenchmarkLedgerError::Open)?;
        initialize(&connection)?;
        Ok(Self { connection })
    }

    pub fn open_in_memory() -> Result<Self, BenchmarkLedgerError> {
        let connection = Connection::open_in_memory().map_err(|_| BenchmarkLedgerError::Open)?;
        initialize(&connection)?;
        Ok(Self { connection })
    }

    pub fn append(
        &mut self,
        result: &RuntimeBenchResult,
    ) -> Result<AppendOutcome, BenchmarkLedgerError> {
        result
            .validate()
            .map_err(|_| BenchmarkLedgerError::InvalidResult)?;
        let coordinate_json =
            serde_json::to_vec(&result.coordinate).map_err(|_| BenchmarkLedgerError::Encode)?;
        let result_json = serde_json::to_vec(result).map_err(|_| BenchmarkLedgerError::Encode)?;
        let coordinate_digest = digest(&coordinate_json);
        let result_digest = digest(&result_json);
        let transaction = self
            .connection
            .transaction()
            .map_err(|_| BenchmarkLedgerError::Write)?;
        let existing = transaction
            .query_row(
                "SELECT result_digest FROM benchmark_results WHERE coordinate_digest = ?1",
                [&coordinate_digest],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|_| BenchmarkLedgerError::Read)?;
        if let Some(existing) = existing {
            return if existing == result_digest {
                Ok(AppendOutcome::AlreadyPresent)
            } else {
                Err(BenchmarkLedgerError::ConflictingResult)
            };
        }
        transaction
            .execute(
                "INSERT INTO benchmark_results (
                    coordinate_digest, result_digest, coordinate_json, result_json
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![
                    coordinate_digest,
                    result_digest,
                    coordinate_json,
                    result_json
                ],
            )
            .map_err(|_| BenchmarkLedgerError::Write)?;
        transaction
            .commit()
            .map_err(|_| BenchmarkLedgerError::Write)?;
        Ok(AppendOutcome::Inserted)
    }

    pub fn results(&self) -> Result<Vec<RuntimeBenchResult>, BenchmarkLedgerError> {
        let mut statement = self
            .connection
            .prepare("SELECT result_json FROM benchmark_results ORDER BY coordinate_digest ASC")
            .map_err(|_| BenchmarkLedgerError::Read)?;
        let rows = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|_| BenchmarkLedgerError::Read)?;
        rows.map(|row| {
            let bytes = row.map_err(|_| BenchmarkLedgerError::Read)?;
            serde_json::from_slice(&bytes).map_err(|_| BenchmarkLedgerError::Corrupt)
        })
        .collect()
    }

    pub fn coverage(&self, plan: &RuntimeBenchPlan) -> Result<PlanCoverage, BenchmarkLedgerError> {
        plan.validate()
            .map_err(|_| BenchmarkLedgerError::InvalidPlan)?;
        let expected = plan
            .coordinates
            .iter()
            .map(coordinate_identity)
            .collect::<Result<HashSet<_>, _>>()?;
        let recorded = self
            .results()?
            .into_iter()
            .map(|result| coordinate_identity(&result.coordinate))
            .collect::<Result<HashSet<_>, _>>()?;
        Ok(PlanCoverage {
            expected: usize_to_u64(expected.len())?,
            recorded: usize_to_u64(recorded.len())?,
            missing: usize_to_u64(expected.difference(&recorded).count())?,
            unexpected: usize_to_u64(recorded.difference(&expected).count())?,
        })
    }
}

fn initialize(connection: &Connection) -> Result<(), BenchmarkLedgerError> {
    connection
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")
        .map_err(|_| BenchmarkLedgerError::Schema)?;
    let application_id = connection
        .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
        .map_err(|_| BenchmarkLedgerError::Schema)?;
    let schema_version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .map_err(|_| BenchmarkLedgerError::Schema)?;
    if application_id == 0 && schema_version == 0 {
        connection
            .execute_batch(&format!(
                "CREATE TABLE benchmark_results (
                    coordinate_digest TEXT PRIMARY KEY NOT NULL,
                    result_digest TEXT NOT NULL,
                    coordinate_json BLOB NOT NULL,
                    result_json BLOB NOT NULL
                 ) STRICT;
                 PRAGMA application_id={APPLICATION_ID};
                 PRAGMA user_version={SCHEMA_VERSION};"
            ))
            .map_err(|_| BenchmarkLedgerError::Schema)?;
    } else if application_id != APPLICATION_ID || schema_version != SCHEMA_VERSION {
        return Err(BenchmarkLedgerError::IncompatibleSchema);
    }
    Ok(())
}

fn coordinate_identity(
    coordinate: &RuntimeBenchCoordinate,
) -> Result<String, BenchmarkLedgerError> {
    coordinate
        .validate()
        .map_err(|_| BenchmarkLedgerError::InvalidResult)?;
    let json = serde_json::to_vec(coordinate).map_err(|_| BenchmarkLedgerError::Encode)?;
    Ok(digest(&json))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn usize_to_u64(value: usize) -> Result<u64, BenchmarkLedgerError> {
    u64::try_from(value).map_err(|_| BenchmarkLedgerError::MetricOverflow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BenchmarkLedgerError {
    #[error("cannot open Runtime benchmark ledger")]
    Open,
    #[error("Runtime benchmark ledger schema cannot be initialized")]
    Schema,
    #[error("Runtime benchmark ledger schema is incompatible")]
    IncompatibleSchema,
    #[error("Runtime benchmark result is invalid")]
    InvalidResult,
    #[error("Runtime benchmark plan is invalid")]
    InvalidPlan,
    #[error("Runtime benchmark artifact cannot be encoded")]
    Encode,
    #[error("Runtime benchmark ledger cannot be read")]
    Read,
    #[error("Runtime benchmark ledger cannot be written")]
    Write,
    #[error("Runtime benchmark ledger contains corrupt evidence")]
    Corrupt,
    #[error("Runtime benchmark coordinate already has different evidence")]
    ConflictingResult,
    #[error("Runtime benchmark coverage metric overflow")]
    MetricOverflow,
}
