//! Backend-independent risk assessment for prepared process invocations.
//!
//! Risk answers whether an operation needs additional consent or must be
//! rejected. It never selects an execution environment and must not infer
//! safety from the presence of a sandbox.

/// Stable risk level frozen before approval and environment selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommandRiskLevel {
    /// Ordinary development activity such as builds, tests, and inspection.
    #[default]
    Routine,
    /// An operation with a meaningful mutation or remote-code boundary.
    Elevated,
    /// An operation whose apparent intent is broad or destructive mutation.
    Destructive,
}

/// Content-safe reason for a non-routine command assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandRiskReason {
    RecursiveDeletion,
    GitDestructiveCleanup,
    RemoteContentExecution,
    PrivilegeEscalation,
}

/// Immutable risk facts attached to one prepared invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandRiskAssessment {
    pub level: CommandRiskLevel,
    pub reasons: Vec<CommandRiskReason>,
}

impl CommandRiskAssessment {
    #[must_use]
    pub const fn routine() -> Self {
        Self {
            level: CommandRiskLevel::Routine,
            reasons: Vec::new(),
        }
    }

    /// Conservatively classify common destructive command forms without
    /// interpreting this result as an execution-environment decision.
    #[must_use]
    pub fn evaluate(command: &str) -> Self {
        let normalized = command.to_ascii_lowercase();
        let mut reasons = Vec::new();
        if normalized.contains("rm -rf") || normalized.contains("rm -fr") {
            reasons.push(CommandRiskReason::RecursiveDeletion);
        }
        if normalized.contains("git clean")
            && (normalized.contains(" -f") || normalized.contains(" --force"))
        {
            reasons.push(CommandRiskReason::GitDestructiveCleanup);
        }
        if (normalized.contains("curl ") || normalized.contains("wget "))
            && normalized.contains('|')
            && (normalized.contains(" sh") || normalized.contains(" bash"))
        {
            reasons.push(CommandRiskReason::RemoteContentExecution);
        }
        if normalized.contains("sudo ") || normalized.starts_with("sudo ") {
            reasons.push(CommandRiskReason::PrivilegeEscalation);
        }
        let level = if reasons.iter().any(|reason| {
            matches!(
                reason,
                CommandRiskReason::RecursiveDeletion | CommandRiskReason::GitDestructiveCleanup
            )
        }) {
            CommandRiskLevel::Destructive
        } else if reasons.is_empty() {
            CommandRiskLevel::Routine
        } else {
            CommandRiskLevel::Elevated
        };
        Self { level, reasons }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/execution_risk.rs"]
mod tests;
