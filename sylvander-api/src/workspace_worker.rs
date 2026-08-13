//! Versioned RPC used by an outbound-connected workspace worker.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const WORKSPACE_WORKER_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWorkerHello {
    pub protocol_version: u16,
    pub target_id: String,
    pub workspace_root: String,
    #[serde(default)]
    pub allow_local_fallback: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceWorkerClientMessage {
    Hello { worker: WorkspaceWorkerHello },
    Event { event: WorkspaceWorkerEvent },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceWorkerServerMessage {
    Welcome { protocol_version: u16 },
    Request { request: WorkspaceWorkerRequest },
    Cancel { request_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWorkerRequest {
    pub request_id: String,
    /// Relative workspace directory below the root granted by the worker.
    pub workspace: String,
    pub operation: WorkspaceWorkerOperation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceWorkerOperation {
    Read {
        path: String,
        max_bytes: Option<usize>,
    },
    Write {
        path: String,
        bytes: Vec<u8>,
    },
    WriteIfRevision {
        path: String,
        expected_sha256: Vec<u8>,
        bytes: Vec<u8>,
        max_bytes: usize,
    },
    List {
        path: String,
        recursive: bool,
        limits: WorkspaceWorkerQueryLimits,
    },
    Search {
        path: String,
        query: String,
        limits: WorkspaceWorkerQueryLimits,
    },
    Command {
        command: String,
        timeout_millis: u64,
        #[serde(default)]
        environment: BTreeMap<String, String>,
        #[serde(default)]
        read_only: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWorkerQueryLimits {
    pub max_results: usize,
    pub max_line_chars: usize,
    pub max_output_bytes: usize,
    pub timeout_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceWorkerEvent {
    Progress {
        request_id: String,
        stream: WorkspaceWorkerStream,
        delta: String,
    },
    Complete {
        request_id: String,
        result: WorkspaceWorkerResult,
    },
    Failed {
        request_id: String,
        code: WorkspaceWorkerErrorCode,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceWorkerStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceWorkerErrorCode {
    InvalidRequest,
    ReadOnly,
    PathBoundary,
    Conflict,
    Timeout,
    Execution,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceWorkerResult {
    Read {
        bytes: Vec<u8>,
        total_bytes: u64,
        truncated: bool,
    },
    Written,
    List {
        entries: Vec<WorkspaceWorkerListEntry>,
        truncated: bool,
    },
    Search {
        matches: Vec<WorkspaceWorkerSearchMatch>,
        truncated: bool,
    },
    Command {
        success: bool,
        status_code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_truncated: bool,
        stderr_truncated: bool,
        stdout_total_bytes: u64,
        stderr_total_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWorkerListEntry {
    pub path: String,
    pub kind: WorkspaceWorkerEntryKind,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceWorkerEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceWorkerSearchMatch {
    pub path: String,
    pub line_number: u64,
    pub line: String,
}
