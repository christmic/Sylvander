use super::*;

impl HttpMemoryIntegrityAnchorConfig {
    pub(crate) fn new_test_http(
        endpoint: &str,
        bearer_token: &[u8],
        timeout: Duration,
        read_retries: u8,
    ) -> Result<Self, MemoryAnchorError> {
        Self::parse(endpoint, bearer_token, timeout, read_retries, true)
    }
}

#[test]
fn file_backend_enforces_create_and_compare_and_swap() {
    let directory = tempfile::tempdir().unwrap();
    let anchor = FileMemoryIntegrityAnchor::new(directory.path().join("anchor.json"));
    assert!(anchor.load().unwrap().is_none());
    let first = anchor.create(b"first").unwrap();
    assert_eq!(anchor.load().unwrap().unwrap().value, b"first");
    assert_eq!(
        anchor.create(b"duplicate"),
        Err(MemoryAnchorError::Conflict)
    );
    let second = anchor.compare_and_swap(&first, b"second").unwrap();
    assert_eq!(anchor.load().unwrap().unwrap().value, b"second");
    assert_eq!(
        anchor.compare_and_swap(&first, b"stale"),
        Err(MemoryAnchorError::Conflict)
    );
    assert_ne!(first, second);
}

#[test]
fn revisions_do_not_disclose_the_stored_value() {
    let revision = file_revision(b"private anchor value");
    assert_eq!(format!("{revision:?}"), "MemoryAnchorRevision([redacted])");
    assert!(!revision.as_str().contains("private"));
}
