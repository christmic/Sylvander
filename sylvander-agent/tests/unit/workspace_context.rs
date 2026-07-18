use super::*;
use crate::workspace_executor::LocalExecutor;

impl<'a> WorkspaceContextSource<'a> {
    fn root(executor: &'a dyn WorkspaceExecutor, target: WorkspaceTarget) -> Self {
        Self {
            executor,
            target,
            focus: PathBuf::new(),
        }
    }
}

async fn discover(
    agent_home: Option<WorkspaceContextSource<'_>>,
    task_workspace: Option<WorkspaceContextSource<'_>>,
) -> Result<Option<String>, WorkspaceExecutorError> {
    Ok(discover_with_report(agent_home, task_workspace)
        .await?
        .prompt)
}

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
        Some(WorkspaceContextSource::root(
            &executor,
            WorkspaceTarget::local(agent.path(), true),
        )),
        Some(WorkspaceContextSource::root(
            &executor,
            WorkspaceTarget::local(task.path(), true),
        )),
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
        Some(WorkspaceContextSource::root(
            &executor,
            WorkspaceTarget::local(root.path(), true),
        )),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(context.contains("canonical"));
    assert!(!context.contains("alias"));
    assert!(!context.contains("outside"));
    assert!(!context.contains(&"x".repeat(100)));
}

#[tokio::test]
async fn instructions_follow_root_to_focus_hierarchy_with_path_provenance() {
    let root = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("src/api")).unwrap();
    std::fs::write(root.path().join("AGENTS.md"), "root guidance").unwrap();
    std::fs::write(root.path().join("src/AGENT.md"), "source guidance").unwrap();
    std::fs::write(root.path().join("src/api/AGENTS.md"), "api canonical").unwrap();
    std::fs::write(root.path().join("src/api/agent.md"), "api alias").unwrap();
    let executor = LocalExecutor;

    let context = discover(
        None,
        Some(WorkspaceContextSource::focused(
            &executor,
            WorkspaceTarget::local(root.path(), true),
            "src/api",
        )),
    )
    .await
    .unwrap()
    .unwrap();

    let root_position = context.find("root guidance").unwrap();
    let source_position = context.find("source guidance").unwrap();
    let api_position = context.find("api canonical").unwrap();
    assert!(root_position < source_position && source_position < api_position);
    assert!(!context.contains("api alias"));
    assert!(context.contains("src/AGENT.md"));
    assert!(context.contains("src/api/AGENTS.md"));
    assert!(
        instruction_ancestors(Path::new("../escape")).is_err(),
        "focus paths never escape the workspace root"
    );
}

#[tokio::test]
async fn skill_manifest_loads_declared_resources_and_reports_capabilities() {
    let root = tempfile::TempDir::new().unwrap();
    let package = root.path().join(".agents/skills/review");
    std::fs::create_dir_all(package.join("references")).unwrap();
    std::fs::write(package.join("SKILL.md"), "review instructions").unwrap();
    std::fs::write(
            package.join("SKILL.toml"),
            "schema_version = 1\nname = \"careful-review\"\nresources = [\"references/checklist.md\"]\n",
        )
        .unwrap();
    std::fs::write(package.join("references/checklist.md"), "review checklist").unwrap();
    let executor = LocalExecutor;

    let context = discover_with_report(
        None,
        Some(WorkspaceContextSource::root(
            &executor,
            WorkspaceTarget::local(root.path(), true),
        )),
    )
    .await
    .unwrap();

    let instructions = context
        .prompt
        .as_ref()
        .unwrap()
        .find("review instructions")
        .unwrap();
    let resource = context
        .prompt
        .as_ref()
        .unwrap()
        .find("review checklist")
        .unwrap();
    assert!(instructions < resource);
    assert_eq!(context.skills[0].name, "careful-review");
    assert_eq!(context.skills[0].status, SkillStatus::Active);
    assert!(
        context.skills[0]
            .capabilities
            .contains(&"declared_resources".to_owned())
    );
}

#[tokio::test]
async fn disabled_or_invalid_skill_packages_never_enter_the_prompt() {
    let root = tempfile::TempDir::new().unwrap();
    let disabled = root.path().join("skills/disabled");
    let empty = root.path().join("skills/empty");
    let invalid = root.path().join("skills/invalid");
    std::fs::create_dir_all(&disabled).unwrap();
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::create_dir_all(&invalid).unwrap();
    std::fs::write(disabled.join("SKILL.md"), "disabled secret").unwrap();
    std::fs::write(
        disabled.join("SKILL.toml"),
        "schema_version = 1\nenabled = false\n",
    )
    .unwrap();
    std::fs::write(empty.join("SKILL.md"), "empty secret").unwrap();
    std::fs::write(empty.join("SKILL.toml"), "").unwrap();
    std::fs::write(invalid.join("SKILL.md"), "invalid secret").unwrap();
    std::fs::write(
        invalid.join("SKILL.toml"),
        "schema_version = 1\nresources = [\"../escape.md\"]\n",
    )
    .unwrap();
    let executor = LocalExecutor;

    let context = discover_with_report(
        None,
        Some(WorkspaceContextSource::root(
            &executor,
            WorkspaceTarget::local(root.path(), true),
        )),
    )
    .await
    .unwrap();

    assert!(context.prompt.is_none());
    assert_eq!(context.skills.len(), 3);
    assert_eq!(context.skills[0].status, SkillStatus::Disabled);
    assert_eq!(context.skills[1].status, SkillStatus::Degraded);
    assert_eq!(context.skills[2].status, SkillStatus::Degraded);
}
