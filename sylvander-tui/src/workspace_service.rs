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
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("SYLVANDER_HOST_SOCKET")
        .env_remove("SYLVANDER_HOST_TOKEN");
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
    let mut diff =
        String::from_utf8(output.stdout).map_err(|_| "git diff output is not valid UTF-8")?;
    if !staged {
        append_untracked_diffs(workspace, &mut diff)?;
    }
    Ok(diff)
}

fn append_untracked_diffs(workspace: &Path, diff: &mut String) -> Result<(), String> {
    let listed = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("SYLVANDER_HOST_SOCKET")
        .env_remove("SYLVANDER_HOST_TOKEN")
        .output()
        .map_err(|error| format!("could not list untracked files: {error}"))?;
    if !listed.status.success() {
        return Err(bound_text(
            String::from_utf8_lossy(&listed.stderr).trim(),
            8 * 1024,
        ));
    }
    if listed.stdout.len() > 256 * 1024 {
        return Err("untracked file list exceeds the 256 KiB inspection limit".into());
    }
    for raw_path in listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|p| !p.is_empty())
    {
        let path =
            std::str::from_utf8(raw_path).map_err(|_| "an untracked path is not valid UTF-8")?;
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("git returned an unsafe untracked path".into());
        }
        let output = Command::new("git")
            .current_dir(workspace)
            .args([
                "--no-pager",
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--no-color",
                "--",
                "/dev/null",
                path,
            ])
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env_remove("SYLVANDER_HOST_SOCKET")
            .env_remove("SYLVANDER_HOST_TOKEN")
            .output()
            .map_err(|error| format!("could not inspect untracked file {path}: {error}"))?;
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(bound_text(
                String::from_utf8_lossy(&output.stderr).trim(),
                8 * 1024,
            ));
        }
        let rendered = String::from_utf8(output.stdout)
            .map_err(|_| format!("diff for untracked file {path} is not valid UTF-8"))?;
        if diff.len().saturating_add(rendered.len()) > MAX_DIFF_BYTES {
            return Err(format!(
                "diff exceeds the {} MiB inspection limit; narrow it in Git first",
                MAX_DIFF_BYTES / 1024 / 1024
            ));
        }
        diff.push_str(&rendered);
    }
    Ok(())
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
#[path = "../tests/unit/workspace_service.rs"]
mod tests;
