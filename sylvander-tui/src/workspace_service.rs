//! Bounded, read-only local workspace queries requested by application actions.

use std::path::Path;
use std::process::Command;

use crate::event::WorkspaceDiffScope;

const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

pub fn load_diff(workspace: &Path, scope: WorkspaceDiffScope) -> Result<String, String> {
    if !workspace.is_dir() {
        return Err(format!("workspace does not exist: {}", workspace.display()));
    }
    match scope {
        WorkspaceDiffScope::Staged => run_git_diff(workspace, true),
        WorkspaceDiffScope::Unstaged => run_git_diff(workspace, false),
        WorkspaceDiffScope::All => {
            let staged = run_git_diff(workspace, true)?;
            let unstaged = run_git_diff(workspace, false)?;
            Ok(join_sections(&staged, &unstaged))
        }
    }
}

fn run_git_diff(workspace: &Path, staged: bool) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(workspace)
        .args(["--no-pager", "diff", "--no-ext-diff", "--no-color"])
        .env("GIT_OPTIONAL_LOCKS", "0");
    if staged {
        command.arg("--cached");
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(bound_text(stderr.trim(), 8 * 1024));
    }
    if output.stdout.len() > MAX_DIFF_BYTES {
        return Err(format!(
            "diff exceeds the {} MiB inspection limit; narrow it in Git first",
            MAX_DIFF_BYTES / 1024 / 1024
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| "git diff output is not valid UTF-8".into())
}

fn join_sections(staged: &str, unstaged: &str) -> String {
    match (staged.is_empty(), unstaged.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!("# Staged changes\n\n{staged}"),
        (true, false) => format!("# Unstaged changes\n\n{unstaged}"),
        (false, false) => {
            format!("# Staged changes\n\n{staged}\n# Unstaged changes\n\n{unstaged}")
        }
    }
}

fn bound_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        text.to_owned()
    } else {
        format!("{}…", String::from_utf8_lossy(&text.as_bytes()[..limit]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_scope_keeps_staged_and_unstaged_changes_distinct() {
        let root = tempfile::tempdir().expect("tempdir");
        run(root.path(), &["init", "-q"]);
        std::fs::write(root.path().join("staged.txt"), "staged\n").expect("write staged");
        run(root.path(), &["add", "staged.txt"]);
        std::fs::write(root.path().join("staged.txt"), "staged\nworking\n")
            .expect("write unstaged");

        let diff = load_diff(root.path(), WorkspaceDiffScope::All).expect("load diff");
        assert!(diff.contains("# Staged changes"));
        assert!(diff.contains("# Unstaged changes"));
        assert!(diff.contains("+staged"));
        assert!(diff.contains("+working"));
    }

    fn run(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success());
    }
}
