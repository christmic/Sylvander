//! Content-safe, machine-readable conformance evidence.

use std::process::Command;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::BenchScenario;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchStatus {
    Passed,
    Failed,
    NotRun,
    NotApplicable,
    InfrastructureError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryState {
    pub sylvander_commit: String,
    pub worktree_dirty: bool,
}

impl RepositoryState {
    pub fn discover() -> Self {
        Self {
            sylvander_commit: git_output(&["rev-parse", "HEAD"]),
            worktree_dirty: !git_output(&["status", "--porcelain"]).is_empty(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassMetrics {
    pub attempts: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub reported_total_tokens: Option<u64>,
    pub counted_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchResult {
    pub schema_version: u32,
    pub run_id: String,
    pub case_id: String,
    pub case_revision: u32,
    pub scenario: BenchScenario,
    pub run_ordinal: u32,
    pub status: BenchStatus,
    pub sylvander_commit: String,
    pub worktree_dirty: bool,
    pub provider_id: String,
    pub protocol: String,
    pub model_id: String,
    pub endpoint_origin: String,
    pub started_at_unix_ms: u64,
    pub duration_ms: u64,
    pub attempts: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_write_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub reported_total_tokens: Option<u64>,
    pub counted_input_tokens: Option<u64>,
    pub failure_kind: Option<String>,
    pub failure_phase: Option<String>,
}

impl BenchResult {
    #[allow(clippy::too_many_arguments)]
    pub fn passed(
        case_id: impl Into<String>,
        case_revision: u32,
        scenario: BenchScenario,
        run_ordinal: u32,
        provider_id: impl Into<String>,
        protocol: impl Into<String>,
        model_id: impl Into<String>,
        endpoint_origin: impl Into<String>,
        started_at_unix_ms: u64,
        duration_ms: u64,
        repository: RepositoryState,
        metrics: PassMetrics,
    ) -> Self {
        let case_id = case_id.into();
        let provider_id = provider_id.into();
        Self {
            schema_version: 1,
            run_id: format!("{provider_id}-{case_id}-{started_at_unix_ms}"),
            case_id,
            case_revision,
            scenario,
            run_ordinal,
            status: BenchStatus::Passed,
            sylvander_commit: repository.sylvander_commit,
            worktree_dirty: repository.worktree_dirty,
            provider_id,
            protocol: protocol.into(),
            model_id: model_id.into(),
            endpoint_origin: endpoint_origin.into(),
            started_at_unix_ms,
            duration_ms,
            attempts: metrics.attempts,
            input_tokens: metrics.input_tokens,
            output_tokens: metrics.output_tokens,
            cache_write_tokens: metrics.cache_write_tokens,
            cache_read_tokens: metrics.cache_read_tokens,
            reasoning_tokens: metrics.reasoning_tokens,
            reported_total_tokens: metrics.reported_total_tokens,
            counted_input_tokens: metrics.counted_input_tokens,
            failure_kind: None,
            failure_phase: None,
        }
    }
}

pub fn endpoint_origin(url: &Url) -> String {
    format!(
        "{}://{}{}",
        url.scheme(),
        url.host_str()
            .expect("validated provider URL must have a host"),
        url.port()
            .map_or_else(String::new, |port| format!(":{port}"))
    )
}

fn git_output(args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .output()
        .expect("git must be available to identify conformance evidence");
    assert!(output.status.success(), "git evidence query must succeed");
    String::from_utf8(output.stdout)
        .expect("git output must be UTF-8")
        .trim()
        .to_owned()
}
