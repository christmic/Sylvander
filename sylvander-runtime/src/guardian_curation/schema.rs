use rusqlite::Connection;

use super::{GuardianCurationError, storage_error};

const APPLICATION_ID: i64 = 1_398_362_965;
const SCHEMA_VERSION: i64 = 1;

pub(super) fn initialize(connection: &mut Connection) -> Result<(), GuardianCurationError> {
    connection
        .execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")
        .map_err(storage_error)?;
    let application_id: i64 = connection
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .map_err(storage_error)?;
    let schema_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(storage_error)?;

    if application_id == 0 && schema_version == 0 {
        let object_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if object_count != 0 {
            return Err(GuardianCurationError::IncompatibleSchema);
        }
        create_latest_schema(connection)?;
    } else if application_id != APPLICATION_ID || schema_version != SCHEMA_VERSION {
        return Err(GuardianCurationError::IncompatibleSchema);
    }
    verify_required_tables(connection)
}

fn create_latest_schema(connection: &mut Connection) -> Result<(), GuardianCurationError> {
    connection
        .execute_batch(
            r"
            BEGIN IMMEDIATE;
            CREATE TABLE guardian_outbox (
              event_id TEXT PRIMARY KEY,
              event_kind TEXT NOT NULL,
              owner_user_id TEXT,
              owner_agent_id TEXT NOT NULL,
              workspace_ids_json TEXT NOT NULL,
              evidence_json TEXT NOT NULL,
              payload_digest TEXT NOT NULL,
              occurred_at INTEGER NOT NULL,
              available_at INTEGER NOT NULL,
              state TEXT NOT NULL CHECK(state IN ('pending','claimed','completed','failed')),
              created_at INTEGER NOT NULL,
              completed_at INTEGER
            ) STRICT;

            CREATE TABLE curator_runs (
              run_id TEXT PRIMARY KEY,
              event_id TEXT NOT NULL UNIQUE REFERENCES guardian_outbox(event_id),
              guardian_service_digest TEXT NOT NULL,
              curator_version TEXT NOT NULL,
              policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
              state TEXT NOT NULL CHECK(state IN ('running','waiting','retryable','succeeded','failed')),
              attempt INTEGER NOT NULL CHECK(attempt > 0),
              claim_token TEXT,
              lease_expires_at INTEGER,
              next_attempt_at INTEGER NOT NULL,
              outcome_code TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            ) STRICT;

            CREATE TABLE memory_candidates (
              candidate_id TEXT PRIMARY KEY,
              run_id TEXT NOT NULL REFERENCES curator_runs(run_id),
              source_key TEXT NOT NULL,
              revision INTEGER NOT NULL CHECK(revision > 0),
              scope TEXT CHECK(scope IS NULL OR scope IN ('relationship','user_profile','agent_canonical','workspace_knowledge')),
              owner_user_id TEXT,
              owner_agent_id TEXT NOT NULL,
              workspace_id TEXT,
              content_json TEXT NOT NULL,
              content_digest TEXT NOT NULL,
              evidence_json TEXT NOT NULL,
              confidence_basis_points INTEGER CHECK(confidence_basis_points IS NULL OR confidence_basis_points BETWEEN 0 AND 10000),
              origin TEXT NOT NULL CHECK(origin IN ('explicit','inferred')),
              sensitivity TEXT CHECK(sensitivity IS NULL OR sensitivity IN ('public','internal','personal','secret')),
              consent_state TEXT NOT NULL CHECK(consent_state IN ('not_required','pending','confirmed','denied')),
              retention_secs INTEGER CHECK(retention_secs IS NULL OR retention_secs > 0),
              dedupe_key TEXT,
              conflict_with TEXT,
              state TEXT NOT NULL CHECK(state IN ('extracted','classified','duplicate','conflict','awaiting_confirmation','policy_pending','authorized','commit_pending','committed','corrected','decayed','forgotten','delivery_failed','rejected')),
              pending_action TEXT CHECK(pending_action IS NULL OR pending_action IN ('commit','correct','decay','forget')),
              expires_at INTEGER,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
              ,UNIQUE(run_id, source_key)
            ) STRICT;
            CREATE INDEX memory_candidates_run_state
              ON memory_candidates(run_id, state, candidate_id);
            CREATE INDEX memory_candidates_dedupe
              ON memory_candidates(owner_agent_id, scope, dedupe_key)
              WHERE dedupe_key IS NOT NULL;

            CREATE TABLE guardian_policy_decisions (
              decision_id TEXT PRIMARY KEY,
              candidate_id TEXT NOT NULL REFERENCES memory_candidates(candidate_id),
              candidate_revision INTEGER NOT NULL,
              action TEXT NOT NULL CHECK(action IN ('commit','correct','decay','forget')),
              policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
              outcome TEXT NOT NULL CHECK(outcome IN ('allow','deny')),
              reason_code TEXT NOT NULL,
              occurred_at INTEGER NOT NULL,
              UNIQUE(candidate_id, candidate_revision, action, policy_revision)
            ) STRICT;

            CREATE TABLE guardian_mutation_outbox (
              mutation_id TEXT PRIMARY KEY,
              candidate_id TEXT NOT NULL REFERENCES memory_candidates(candidate_id),
              candidate_revision INTEGER NOT NULL,
              action TEXT NOT NULL CHECK(action IN ('commit','correct','decay','forget')),
              scope TEXT NOT NULL CHECK(scope IN ('relationship','user_profile','agent_canonical','workspace_knowledge')),
              owner_user_id TEXT,
              owner_agent_id TEXT NOT NULL,
              workspace_id TEXT,
              body_json TEXT NOT NULL,
              body_digest TEXT NOT NULL,
              idempotency_key TEXT NOT NULL UNIQUE,
              state TEXT NOT NULL CHECK(state IN ('pending','claimed','completed','dead_letter')),
              attempt INTEGER NOT NULL CHECK(attempt >= 0),
              available_at INTEGER NOT NULL,
              claim_token TEXT,
              lease_expires_at INTEGER,
              last_error_code TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL,
              completed_at INTEGER,
              UNIQUE(candidate_id, candidate_revision, action)
            ) STRICT;
            CREATE INDEX guardian_mutation_delivery
              ON guardian_mutation_outbox(state, available_at, mutation_id);

            CREATE TABLE guardian_curation_audit (
              sequence INTEGER PRIMARY KEY AUTOINCREMENT,
              audit_id TEXT NOT NULL UNIQUE,
              occurred_at INTEGER NOT NULL,
              event_id TEXT,
              run_id TEXT,
              candidate_id TEXT,
              mutation_id TEXT,
              guardian_service_digest TEXT NOT NULL,
              operation TEXT NOT NULL,
              from_state TEXT,
              to_state TEXT,
              reason_code TEXT NOT NULL,
              record_digest TEXT
            ) STRICT;

            CREATE TABLE capability_invocation_audit (
              invocation_id TEXT NOT NULL,
              phase TEXT NOT NULL CHECK(phase IN ('authorized','completed')),
              actor TEXT NOT NULL CHECK(actor IN ('worker','guardian')),
              capability TEXT NOT NULL,
              capability_revision TEXT NOT NULL,
              policy_revision INTEGER NOT NULL CHECK(policy_revision > 0),
              owner_digest TEXT NOT NULL,
              outcome TEXT NOT NULL CHECK(outcome IN ('allowed','succeeded','failed')),
              PRIMARY KEY(invocation_id, phase)
            ) STRICT;

            PRAGMA application_id=1398362965;
            PRAGMA user_version=1;
            COMMIT;
            ",
        )
        .map_err(storage_error)
}

fn verify_required_tables(connection: &Connection) -> Result<(), GuardianCurationError> {
    for table in [
        "guardian_outbox",
        "curator_runs",
        "memory_candidates",
        "guardian_policy_decisions",
        "guardian_mutation_outbox",
        "guardian_curation_audit",
        "capability_invocation_audit",
    ] {
        let present: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .map_err(storage_error)?;
        if present != 1 {
            return Err(GuardianCurationError::IncompatibleSchema);
        }
    }
    Ok(())
}
