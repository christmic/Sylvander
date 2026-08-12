use super::*;

#[test]
fn writes_content_under_the_managed_root() {
    let root = tempfile::tempdir().expect("tempdir");
    let disk = FilesystemToolResultDisk::new(root.path()).expect("disk");

    let handle = disk.persist("toolu_abc-1", "hello world").expect("persist");

    assert_eq!(handle.original_bytes, 11);
    assert_eq!(fs::read_to_string(handle.path).unwrap(), "hello world");
}

#[test]
fn rejects_identifiers_that_can_escape_the_managed_root() {
    let root = tempfile::tempdir().expect("tempdir");
    let disk = FilesystemToolResultDisk::new(root.path()).expect("disk");

    for invalid in ["", ".", "..", "../outside", "nested/file", "bad\\file"] {
        assert_eq!(
            disk.persist(invalid, "body").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[test]
fn repeated_tool_identifier_replaces_its_own_artifact() {
    let root = tempfile::tempdir().expect("tempdir");
    let disk = FilesystemToolResultDisk::new(root.path()).expect("disk");

    let first = disk.persist("same", "first").expect("first");
    let second = disk.persist("same", "second").expect("second");

    assert_eq!(first.path, second.path);
    assert_eq!(fs::read_to_string(second.path).unwrap(), "second");
}
