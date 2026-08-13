//! Content-safe normalized verifier result.

use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::matrix::AgentMatrixCoordinate;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentBenchStatus {
    Passed,
    Failed,
    AgentError,
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
    #[must_use]
    pub fn discover() -> Self {
        Self {
            sylvander_commit: git_output(&["rev-parse", "HEAD"]),
            worktree_dirty: !git_output(&["status", "--porcelain"]).is_empty(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentBenchResult {
    pub schema_version: u32,
    #[serde(flatten)]
    pub coordinate: AgentMatrixCoordinate,
    pub status: AgentBenchStatus,
    pub reward: Option<f64>,
    pub sylvander_commit: String,
    pub worktree_dirty: bool,
    pub harness_revision: String,
    pub duration_ms: u64,
    pub iterations: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: Option<u64>,
    pub failure_kind: Option<String>,
}

impl AgentBenchResult {
    #[allow(clippy::too_many_arguments)]
    pub fn recorded(
        coordinate: AgentMatrixCoordinate,
        status: AgentBenchStatus,
        reward: Option<f64>,
        repository: RepositoryState,
        harness_revision: impl Into<String>,
        duration_ms: u64,
        iterations: u32,
        tool_calls: u32,
        input_tokens: u64,
        output_tokens: u64,
        cached_tokens: Option<u64>,
        failure_kind: Option<String>,
    ) -> Result<Self, &'static str> {
        if reward.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
            return Err("verifier reward must be finite and between zero and one");
        }
        if matches!(status, AgentBenchStatus::Passed | AgentBenchStatus::Failed) != reward.is_some()
        {
            return Err("executed Agent results must retain the verifier reward");
        }
        Ok(Self {
            schema_version: 1,
            coordinate,
            status,
            reward,
            sylvander_commit: repository.sylvander_commit,
            worktree_dirty: repository.worktree_dirty,
            harness_revision: harness_revision.into(),
            duration_ms,
            iterations,
            tool_calls,
            input_tokens,
            output_tokens,
            cached_tokens,
            failure_kind,
        })
    }
}

fn git_output(arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(arguments)
        .output()
        .expect("git is required for benchmark evidence");
    assert!(output.status.success(), "git evidence query must succeed");
    String::from_utf8(output.stdout)
        .expect("git output must be UTF-8")
        .trim()
        .to_owned()
}
