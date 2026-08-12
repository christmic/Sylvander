use super::*;
use crate::test_support::InMemoryToolResultDisk;

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
    let mem: Box<dyn ToolResultDisk> = Box::new(InMemoryToolResultDisk::new());

    // Smoke: Runtime-selected implementations remain callable by the layer.
    let _ = mem.persist("x", "y").unwrap();
}
