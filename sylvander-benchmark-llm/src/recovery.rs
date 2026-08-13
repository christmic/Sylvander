//! Runtime-owned durable interruption measured through a killed child process.

use std::io::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sylvander_runtime::evidence::{EvidenceStore, TurnStart};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use url::Url;

use crate::{
    BenchObservation, BenchResult, BenchStatus, MatrixCell, PassMetrics, ProtocolBinding,
    RepositoryState, endpoint_origin,
};

pub async fn run_process_interruption_cell(
    binding: &ProtocolBinding,
    cell: &MatrixCell,
    repository: RepositoryState,
) -> BenchResult {
    let started_at = now_unix_millis();
    let started = Instant::now();
    let origin = Url::parse(&binding.base_url)
        .map(|url| endpoint_origin(&url))
        .unwrap_or_default();
    let outcome = process_interruption().await;
    let (status, observation) = match outcome {
        Ok(()) => (
            BenchStatus::Passed,
            BenchObservation {
                metrics: PassMetrics {
                    attempts: 1,
                    ..PassMetrics::default()
                },
                ..BenchObservation::default()
            },
        ),
        Err(kind) => (
            BenchStatus::InfrastructureError,
            BenchObservation {
                failure_kind: Some(kind.into()),
                ..BenchObservation::default()
            },
        ),
    };
    BenchResult::recorded(
        cell,
        1,
        status,
        origin,
        started_at,
        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        repository,
        observation,
    )
}

pub async fn run_crash_fixture(path: &Path) -> Result<(), String> {
    let store = EvidenceStore::open(path)
        .await
        .map_err(|_| "cannot open crash-fixture evidence store")?;
    store
        .start_run("bench-run".into(), "llm-process-interruption".into(), 1)
        .await
        .map_err(|_| "cannot start crash-fixture run")?;
    store
        .start_turn(TurnStart {
            id: "bench-turn".into(),
            run_id: "bench-run".into(),
            session_id: "bench-session".into(),
            agent_id: None,
            started_at: 2,
            input_bytes: 0,
            input_digest: None,
        })
        .await
        .map_err(|_| "cannot start crash-fixture turn")?;
    println!("READY");
    std::io::stdout()
        .flush()
        .map_err(|_| "cannot signal crash-fixture readiness")?;
    std::future::pending::<()>().await;
    Ok(())
}

async fn process_interruption() -> Result<(), &'static str> {
    let directory = tempfile::tempdir().map_err(|_| "cannot_create_recovery_directory")?;
    let database = directory.path().join("evidence.db");
    let executable = std::env::current_exe().map_err(|_| "cannot_resolve_bench_executable")?;
    let mut child = tokio::process::Command::new(executable)
        .arg("crash-fixture")
        .arg(&database)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| "cannot_spawn_crash_fixture")?;
    let stdout = child.stdout.take().ok_or("cannot_capture_crash_fixture")?;
    let mut ready = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        BufReader::new(stdout).read_line(&mut ready),
    )
    .await
    .map_err(|_| "crash_fixture_readiness_timeout")?
    .map_err(|_| "crash_fixture_readiness_failed")?;
    if ready.trim() != "READY" {
        return Err("crash_fixture_not_ready");
    }
    child
        .kill()
        .await
        .map_err(|_| "cannot_kill_crash_fixture")?;
    let _ = child.wait().await;

    let reopened = EvidenceStore::open(&database)
        .await
        .map_err(|_| "cannot_reopen_recovery_evidence")?;
    let status = reopened
        .turn_status("bench-turn".into())
        .await
        .map_err(|_| "cannot_read_recovered_turn")?;
    if status.as_deref() != Some("interrupted") {
        return Err("recovered_turn_was_not_interrupted");
    }
    Ok(())
}

fn now_unix_millis() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
