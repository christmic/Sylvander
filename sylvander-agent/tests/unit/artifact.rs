use super::*;

fn assert_object_safe(_: Box<dyn TurnArtifactStore>) {}

#[test]
fn values_do_not_expose_storage_location_types() {
    let write = ArtifactWrite {
        call_id: "call-1".to_string(),
        media_type: "text/plain; charset=utf-8".to_string(),
        payload: b"value".to_vec(),
    };
    let reference = ArtifactReference {
        locator: "artifact:opaque".to_string(),
        original_bytes: write.payload.len(),
    };

    assert_eq!(reference.locator, "artifact:opaque");
    assert_eq!(reference.original_bytes, 5);
}

#[test]
fn turn_store_remains_dyn_compatible() {
    assert_object_safe(Box::new(crate::test_support::InMemoryArtifactStore::new()));
}
