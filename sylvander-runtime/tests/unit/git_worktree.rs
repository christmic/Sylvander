use super::*;

fn repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git_ok(repo.path(), &["init", "-b", "master"]).unwrap();
    fs::write(repo.path().join("tracked.txt"), "before\n").unwrap();
    git_ok(repo.path(), &["add", "."]).unwrap();
    git_ok(
        repo.path(),
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "initial",
        ],
    )
    .unwrap();
    repo
}

#[test]
fn create_edit_inspect_and_accept_merge() {
    let repo = repo();
    let state = tempfile::tempdir().unwrap();
    let manager = GitWorktreeManager::new(state.path());
    let lease = manager.create("session-1", repo.path()).unwrap();
    fs::write(lease.effective_workspace.join("tracked.txt"), "after\n").unwrap();
    fs::write(lease.effective_workspace.join("new.txt"), "new\n").unwrap();
    let diff = manager.inspect(&lease).unwrap();
    assert!(diff.status.contains("M tracked.txt"));
    assert!(diff.status.contains("?? new.txt"));
    assert!(diff.patch.contains("+after"));
    assert!(diff.patch.contains("+new"));
    manager.accept(&lease).unwrap();
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "after\n"
    );
    assert!(lease.worktree_root.exists());
    manager.discard(&lease).unwrap();
}

#[test]
fn discard_removes_changes_without_touching_source() {
    let repo = repo();
    let state = tempfile::tempdir().unwrap();
    let manager = GitWorktreeManager::new(state.path());
    let lease = manager.create("session-2", repo.path()).unwrap();
    fs::write(lease.effective_workspace.join("tracked.txt"), "discarded\n").unwrap();
    manager.discard(&lease).unwrap();
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "before\n"
    );
    assert!(manager.open("session-2").is_err());
}

#[test]
fn reviewed_merge_can_be_reverted_only_before_source_advances() {
    let repo = repo();
    let state = tempfile::tempdir().unwrap();
    let manager = GitWorktreeManager::new(state.path());
    let lease = manager.create("experiment-1", repo.path()).unwrap();
    fs::write(lease.effective_workspace.join("tracked.txt"), "candidate\n").unwrap();
    let reviewed = manager.accept_reviewed(&lease).unwrap().unwrap();
    assert_ne!(reviewed.previous_commit, reviewed.merge_commit);
    assert_ne!(reviewed.candidate_commit, reviewed.merge_commit);
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "candidate\n"
    );
    let rollback_commit = manager
        .rollback_reviewed(&lease, &reviewed.merge_commit)
        .unwrap();
    assert_ne!(rollback_commit, reviewed.merge_commit);
    assert_eq!(
        fs::read_to_string(repo.path().join("tracked.txt")).unwrap(),
        "before\n"
    );
    assert!(
        manager
            .rollback_reviewed(&lease, &reviewed.merge_commit)
            .is_err()
    );
    manager.discard(&lease).unwrap();
}

#[test]
fn reconcile_retains_active_and_removes_deleted_session_leases() {
    let repo = repo();
    let state = tempfile::tempdir().unwrap();
    let manager = GitWorktreeManager::new(state.path());
    let active = manager.create("active", repo.path()).unwrap();
    let deleted = manager.create("stale", repo.path()).unwrap();
    let report = manager
        .reconcile(&HashMap::from([(
            active.session_id.clone(),
            active.effective_workspace.clone(),
        )]))
        .unwrap();

    assert_eq!(
        report,
        WorktreeReconciliation {
            retained: 1,
            removed: 1
        }
    );
    assert!(active.worktree_root.is_dir());
    assert!(!deleted.worktree_root.exists());
    assert!(manager.open("active").is_ok());
    assert!(manager.open("stale").is_err());
    manager.discard(&active).unwrap();
}

#[test]
fn reconcile_removes_worktree_left_before_manifest_commit() {
    let repo = repo();
    let state = tempfile::tempdir().unwrap();
    let manager = GitWorktreeManager::new(state.path());
    let orphan = manager.create("orphan", repo.path()).unwrap();
    fs::remove_file(manager.manifest_path("orphan")).unwrap();

    let report = manager.reconcile(&HashMap::new()).unwrap();

    assert_eq!(report.retained, 0);
    assert_eq!(report.removed, 1);
    assert!(!orphan.worktree_root.exists());
    let branch = git_output(
        repo.path(),
        &[
            "show-ref",
            "--verify",
            "--quiet",
            "refs/heads/sylvander/orphan",
        ],
    )
    .unwrap();
    assert!(!branch.status.success());
}

#[test]
fn reconcile_rejects_missing_or_mismatched_active_lease() {
    let repo = repo();
    let state = tempfile::tempdir().unwrap();
    let manager = GitWorktreeManager::new(state.path());
    let lease = manager.create("active", repo.path()).unwrap();

    let mismatch = manager
        .reconcile(&HashMap::from([(
            lease.session_id.clone(),
            repo.path().to_path_buf(),
        )]))
        .unwrap_err();
    assert!(mismatch.contains("does not match"));

    manager.discard(&lease).unwrap();
    let missing = manager
        .reconcile(&HashMap::from([(
            "missing".into(),
            repo.path().to_path_buf(),
        )]))
        .unwrap_err();
    assert!(missing.contains("missing worktree lease"));
}
