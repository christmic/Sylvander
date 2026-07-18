use super::*;
use crate::test_support::InMemoryToolResultDisk;

#[test]
fn filesystem_disk_writes_and_returns_handle() {
    let dir = tempfile::tempdir().expect("tempdir");
    let disk = FilesystemToolResultDisk::with_root(dir.path().to_path_buf()).expect("disk");

    let handle = disk
        .persist("toolu_abc", "hello world")
        .expect("persist should succeed");

    assert_eq!(handle.original_bytes, 11);
    assert!(handle.path.exists());

    let read_back = std::fs::read_to_string(&handle.path).expect("read back");
    assert_eq!(read_back, "hello world");
}

#[test]
fn filesystem_disk_path_for_is_predictable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let disk = FilesystemToolResultDisk::with_root(dir.path().to_path_buf()).expect("disk");

    let p = disk.path_for("toolu_xyz");
    assert!(p.ends_with("toolu_xyz.txt"));
}

#[test]
fn in_memory_disk_records_writes() {
    let disk = InMemoryToolResultDisk::new();

    let h1 = disk.persist("a", "alpha").expect("persist a");
    let h2 = disk.persist("b", "beta beta").expect("persist b");

    assert_eq!(h1.original_bytes, 5);
    assert_eq!(h2.original_bytes, 9);
    assert_eq!(disk.write_count(), 2);
    assert_eq!(disk.ids(), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(disk.get("a").as_deref(), Some("alpha"));
    assert_eq!(disk.get("b").as_deref(), Some("beta beta"));
    assert_eq!(disk.get("missing"), None);
}

#[test]
fn in_memory_disk_overwrites_on_same_id() {
    let disk = InMemoryToolResultDisk::new();
    disk.persist("dup", "first").unwrap();
    disk.persist("dup", "second").unwrap();

    assert_eq!(disk.write_count(), 2);
    assert_eq!(disk.get("dup").as_deref(), Some("second"));
}

#[test]
fn trait_is_object_safe() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let fs: Box<dyn ToolResultDisk> =
        Box::new(FilesystemToolResultDisk::with_root(tmp.path().to_path_buf()).unwrap());
    let mem: Box<dyn ToolResultDisk> = Box::new(InMemoryToolResultDisk::new());

    // Smoke: both impls callable through trait object.
    let _ = fs.persist("x", "y").unwrap();
    let _ = mem.persist("x", "y").unwrap();
}
