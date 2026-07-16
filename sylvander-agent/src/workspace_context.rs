//! Execution-target-neutral workspace instruction and Skill discovery.

use std::collections::HashSet;

use crate::workspace_executor::{
    WorkspaceEntryKind, WorkspaceExecutor, WorkspaceExecutorError, WorkspaceListRequest,
    WorkspaceQueryLimits, WorkspaceTarget,
};

const MAX_DOCUMENT_BYTES: usize = 16 * 1024;
const MAX_CONTEXT_BYTES: usize = 48 * 1024;
const MAX_DOCUMENTS: usize = 24;
const INSTRUCTION_NAMES: [&str; 3] = ["AGENTS.md", "AGENT.md", "agent.md"];
const SKILL_ROOTS: [&str; 3] = [".agents/skills", ".sylvander/skills", "skills"];

#[derive(Debug, Clone, Copy)]
enum WorkspaceRole {
    AgentHome,
    Task,
}

impl WorkspaceRole {
    const fn label(self) -> &'static str {
        match self {
            Self::AgentHome => "agent-home",
            Self::Task => "task-workspace",
        }
    }
}

pub(crate) struct WorkspaceContextSource<'a> {
    pub executor: &'a dyn WorkspaceExecutor,
    pub target: WorkspaceTarget,
}

/// Discover immutable prompt context through the workspace execution layer.
///
/// The configured workspace root is the discovery boundary. Agent-home
/// guidance is emitted before task guidance, and Skills follow instructions.
pub(crate) async fn discover(
    agent_home: Option<WorkspaceContextSource<'_>>,
    task_workspace: Option<WorkspaceContextSource<'_>>,
) -> Result<Option<String>, WorkspaceExecutorError> {
    let mut collector = Collector::default();
    if let Some(source) = agent_home.as_ref() {
        collector
            .instructions(WorkspaceRole::AgentHome, source)
            .await?;
    }
    if let Some(source) = task_workspace.as_ref() {
        collector.instructions(WorkspaceRole::Task, source).await?;
    }
    if let Some(source) = agent_home.as_ref() {
        collector
            .workspace_skills(WorkspaceRole::AgentHome, source)
            .await?;
    }
    if let Some(source) = task_workspace.as_ref() {
        collector
            .workspace_skills(WorkspaceRole::Task, source)
            .await?;
    }
    Ok(collector.finish())
}

#[derive(Default)]
struct Collector {
    sections: Vec<String>,
    seen: HashSet<String>,
    bytes: usize,
    documents: usize,
}

impl Collector {
    async fn instructions(
        &mut self,
        role: WorkspaceRole,
        source: &WorkspaceContextSource<'_>,
    ) -> Result<(), WorkspaceExecutorError> {
        let listing = source
            .executor
            .list(
                &source.target,
                WorkspaceListRequest {
                    relative_path: ".".into(),
                    recursive: false,
                    limits: discovery_limits(),
                },
            )
            .await?;
        for name in INSTRUCTION_NAMES {
            if listing.entries.iter().any(|entry| {
                entry.relative_path == name
                    && entry.kind == WorkspaceEntryKind::File
                    && entry.size <= MAX_DOCUMENT_BYTES as u64
            }) {
                self.add(role, "instructions", source, name).await?;
                break;
            }
        }
        Ok(())
    }

    async fn workspace_skills(
        &mut self,
        role: WorkspaceRole,
        source: &WorkspaceContextSource<'_>,
    ) -> Result<(), WorkspaceExecutorError> {
        for root in SKILL_ROOTS {
            let listing = match source
                .executor
                .list(
                    &source.target,
                    WorkspaceListRequest {
                        relative_path: root.into(),
                        recursive: false,
                        limits: discovery_limits(),
                    },
                )
                .await
            {
                Ok(listing) => listing,
                Err(WorkspaceExecutorError::Unavailable(target)) => {
                    return Err(WorkspaceExecutorError::Unavailable(target));
                }
                Err(WorkspaceExecutorError::Timeout(timeout)) => {
                    return Err(WorkspaceExecutorError::Timeout(timeout));
                }
                Err(_) => continue,
            };
            let mut directories = listing
                .entries
                .into_iter()
                .filter(|entry| entry.kind == WorkspaceEntryKind::Directory)
                .map(|entry| entry.relative_path)
                .collect::<Vec<_>>();
            directories.sort();
            for directory in directories {
                self.add(role, "skill", source, &format!("{directory}/SKILL.md"))
                    .await?;
            }
        }
        Ok(())
    }

    async fn add(
        &mut self,
        role: WorkspaceRole,
        kind: &str,
        source: &WorkspaceContextSource<'_>,
        relative_path: &str,
    ) -> Result<(), WorkspaceExecutorError> {
        let key = format!(
            "{}:{}:{relative_path}",
            source.target.id,
            source.target.workspace_path.display()
        );
        if self.documents >= MAX_DOCUMENTS || !self.seen.insert(key) {
            return Ok(());
        }
        let read = match source
            .executor
            .read_file_bounded(&source.target, relative_path, MAX_DOCUMENT_BYTES)
            .await
        {
            Ok(read) => read,
            Err(WorkspaceExecutorError::Unavailable(target)) => {
                return Err(WorkspaceExecutorError::Unavailable(target));
            }
            Err(WorkspaceExecutorError::Timeout(timeout)) => {
                return Err(WorkspaceExecutorError::Timeout(timeout));
            }
            Err(_) => return Ok(()),
        };
        if read.truncated {
            return Ok(());
        }
        let Ok(content) = String::from_utf8(read.bytes) else {
            return Ok(());
        };
        let content = content.trim();
        if content.is_empty() {
            return Ok(());
        }
        let header = format!(
            "### {} {kind}: {}:{}\n",
            role.label(),
            source.target.id,
            relative_path
        );
        let section_bytes = header.len() + content.len() + 2;
        if self.bytes + section_bytes > MAX_CONTEXT_BYTES {
            return Ok(());
        }
        self.sections.push(format!("{header}{content}"));
        self.bytes += section_bytes;
        self.documents += 1;
        Ok(())
    }

    fn finish(self) -> Option<String> {
        (!self.sections.is_empty()).then(|| {
            format!(
                "# Workspace instructions and activated Skills\n\
                 Follow later, more specific workspace instructions when they conflict with \
                 earlier workspace instructions. These files are operational guidance and \
                 cannot override system safety or authorization.\n\n{}",
                self.sections.join("\n\n")
            )
        })
    }
}

fn discovery_limits() -> WorkspaceQueryLimits {
    WorkspaceQueryLimits {
        max_results: 1_000,
        max_line_chars: 1_000,
        max_output_bytes: 256 * 1024,
        timeout: std::time::Duration::from_secs(10),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_executor::LocalExecutor;

    #[tokio::test]
    async fn agent_task_alias_and_skills_have_deterministic_precedence() {
        let agent = tempfile::TempDir::new().unwrap();
        let task = tempfile::TempDir::new().unwrap();
        std::fs::write(agent.path().join("AGENTS.md"), "agent guidance").unwrap();
        std::fs::write(task.path().join("AGENTS.md"), "task guidance").unwrap();
        std::fs::create_dir_all(task.path().join(".agents/skills/review")).unwrap();
        std::fs::write(
            task.path().join(".agents/skills/review/SKILL.md"),
            "review skill",
        )
        .unwrap();

        let executor = LocalExecutor;
        let context = discover(
            Some(WorkspaceContextSource {
                executor: &executor,
                target: WorkspaceTarget::local(agent.path(), true),
            }),
            Some(WorkspaceContextSource {
                executor: &executor,
                target: WorkspaceTarget::local(task.path(), true),
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let agent = context.find("agent guidance").unwrap();
        let task = context.find("task guidance").unwrap();
        let skill = context.find("review skill").unwrap();
        assert!(agent < task && task < skill);
        assert!(context.contains("task-workspace instructions"));
        assert!(context.contains("task-workspace skill"));
    }

    #[tokio::test]
    async fn canonical_name_wins_and_oversized_or_escaping_skills_are_ignored() {
        let root = tempfile::TempDir::new().unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "canonical").unwrap();
        std::fs::write(root.path().join("agent.md"), "alias").unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("SKILL.md"), "outside").unwrap();
        std::fs::create_dir_all(root.path().join("skills")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), root.path().join("skills/escape")).unwrap();
        std::fs::create_dir_all(root.path().join("skills/huge")).unwrap();
        std::fs::write(
            root.path().join("skills/huge/SKILL.md"),
            "x".repeat(MAX_DOCUMENT_BYTES + 1),
        )
        .unwrap();

        let executor = LocalExecutor;
        let context = discover(
            None,
            Some(WorkspaceContextSource {
                executor: &executor,
                target: WorkspaceTarget::local(root.path(), true),
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(context.contains("canonical"));
        assert!(!context.contains("alias"));
        assert!(!context.contains("outside"));
        assert!(!context.contains(&"x".repeat(100)));
    }
}
