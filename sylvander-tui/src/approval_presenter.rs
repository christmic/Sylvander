//! Human-facing risk and scope summaries for approval requests.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
            Self::Critical => "CRITICAL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalSummary {
    pub risk: RiskLevel,
    pub action: String,
    pub scope: String,
}

pub fn summarize(tool_name: &str, input: &Value) -> ApprovalSummary {
    let normalized = tool_name.to_ascii_lowercase();
    let action = crate::tool_presenter::compact_target(tool_name, input);
    let scope = scope(input);
    let risk = match normalized.as_str() {
        "read" | "read_file" | "search" | "grep" | "rg" | "read_memory" | "memory_read" => {
            RiskLevel::Low
        }
        "write" | "write_file" | "edit" | "edit_file" | "write_memory" | "memory_write" => {
            RiskLevel::Medium
        }
        "bash" | "shell" | "exec" => shell_risk(input),
        "ask_user" => RiskLevel::Low,
        _ => RiskLevel::High,
    };
    ApprovalSummary {
        risk,
        action,
        scope,
    }
}

fn shell_risk(input: &Value) -> RiskLevel {
    let command = input
        .get("command")
        .or_else(|| input.get("cmd"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let critical = [
        "rm -rf",
        "sudo ",
        "chmod -r",
        "chown -r",
        "mkfs",
        "diskutil erase",
        "git reset --hard",
        "git clean -fd",
        "curl | sh",
        "wget | sh",
    ];
    if critical.iter().any(|pattern| command.contains(pattern)) {
        RiskLevel::Critical
    } else {
        RiskLevel::High
    }
}

fn scope(input: &Value) -> String {
    for key in ["path", "file_path", "cwd", "workdir", "directory"] {
        if let Some(value) = input.get(key).and_then(Value::as_str) {
            return value.to_string();
        }
    }
    "current workspace".into()
}

#[cfg(test)]
#[path = "../tests/unit/approval_presenter.rs"]
mod tests;
