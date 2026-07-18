use super::*;
use tempfile::TempDir;

#[test]
fn multiple_edits_in_one_turn_roll_back_in_reverse_order() {
    let root = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let file = root.path().join("src.txt");
    fs::write(&file, "zero").unwrap();
    let journal = WorkspaceJournal::new(data.path());

    let first = journal
        .prepare("s", "turn-1", root.path(), "src.txt", b"one")
        .unwrap();
    fs::write(&file, "one").unwrap();
    journal.commit(&first).unwrap();
    let second = journal
        .prepare("s", "turn-1", root.path(), "src.txt", b"two")
        .unwrap();
    fs::write(&file, "two").unwrap();
    journal.commit(&second).unwrap();

    let preview = journal.preview_latest_turn("s").unwrap();
    let report = journal.rollback_latest_turn("s", &preview.turn_id).unwrap();
    assert_eq!(report.turn_id, "turn-1");
    assert_eq!(fs::read_to_string(file).unwrap(), "zero");
}

#[test]
fn conflict_refuses_to_overwrite_external_changes() {
    let root = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let file = root.path().join("src.txt");
    fs::write(&file, "before").unwrap();
    let journal = WorkspaceJournal::new(data.path());
    let mutation = journal
        .prepare("s", "turn-1", root.path(), "src.txt", b"agent")
        .unwrap();
    fs::write(&file, "agent").unwrap();
    journal.commit(&mutation).unwrap();
    fs::write(&file, "external").unwrap();

    assert!(
        journal
            .rollback_latest_turn("s", "turn-1")
            .unwrap_err()
            .contains("conflict")
    );
    assert_eq!(fs::read_to_string(file).unwrap(), "external");
}

#[test]
fn prepared_crash_record_and_new_file_are_recoverable() {
    let root = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    let file = root.path().join("new.txt");
    let journal = WorkspaceJournal::new(data.path());
    let _pending = journal
        .prepare("s", "turn-1", root.path(), "new.txt", b"created")
        .unwrap();
    fs::write(&file, "created").unwrap();

    journal.rollback_latest_turn("s", "turn-1").unwrap();
    assert!(!file.exists());
}

#[cfg(unix)]
#[test]
fn resolver_rejects_parent_escape_and_symlink_hops() {
    use std::os::unix::fs::symlink;
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    symlink(outside.path(), root.path().join("link")).unwrap();
    assert!(WorkspaceJournal::resolve(root.path(), "../outside").is_err());
    assert!(WorkspaceJournal::resolve(root.path(), "link/file").is_err());
}
