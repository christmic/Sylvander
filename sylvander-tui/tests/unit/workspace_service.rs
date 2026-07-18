use super::*;

#[test]
fn all_scope_keeps_staged_and_unstaged_changes_distinct() {
    let root = tempfile::tempdir().expect("tempdir");
    run(root.path(), &["init", "-q"]);
    std::fs::write(root.path().join("staged.txt"), "staged\n").expect("write staged");
    run(root.path(), &["add", "staged.txt"]);
    std::fs::write(root.path().join("staged.txt"), "staged\nworking\n").expect("write unstaged");
    std::fs::write(root.path().join("untracked.txt"), "untracked\n").expect("write untracked");

    let diff = load_diff(root.path(), WorkspaceDiffScope::All).expect("load diff");
    assert!(diff.contains("# Staged changes"));
    assert!(diff.contains("# Unstaged changes"));
    assert!(diff.contains("+staged"));
    assert!(diff.contains("+working"));
    assert!(diff.contains("+untracked"));
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
