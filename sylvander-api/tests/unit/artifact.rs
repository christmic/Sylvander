use crate::{
    ARTIFACT_READ_PROTOCOL_VERSION, ArtifactChunk, ArtifactReadRequest, MAX_ARTIFACT_READ_BYTES,
    UiClientMessage, UiServerMessage,
};

#[test]
fn artifact_read_wire_shape_is_versioned_and_bounded() {
    assert_eq!(MAX_ARTIFACT_READ_BYTES, 48 * 1024);
    let request = UiClientMessage::ReadArtifact {
        request: ArtifactReadRequest {
            version: ARTIFACT_READ_PROTOCOL_VERSION,
            session_id: "session-a".into(),
            locator: "artifact:opaque".into(),
            offset: 7,
        },
    };
    let value = serde_json::to_value(request).expect("serialize request");
    assert_eq!(value["type"], "read_artifact");
    assert_eq!(value["request"]["offset"], 7);

    let response = UiServerMessage::ArtifactChunk {
        chunk: ArtifactChunk {
            version: ARTIFACT_READ_PROTOCOL_VERSION,
            session_id: "session-a".into(),
            locator: "artifact:opaque".into(),
            media_type: "text/plain".into(),
            offset: 7,
            total_bytes: 10,
            next_offset: 10,
            eof: true,
            content_base64: "YWJj".into(),
            payload_digest_sha256: "digest".into(),
        },
    };
    let value = serde_json::to_value(response).expect("serialize response");
    assert_eq!(value["type"], "artifact_chunk");
    assert_eq!(value["chunk"]["content_base64"], "YWJj");
}

#[test]
fn artifact_request_rejects_unknown_owner_selector() {
    let error = serde_json::from_value::<ArtifactReadRequest>(serde_json::json!({
        "version": ARTIFACT_READ_PROTOCOL_VERSION,
        "session_id": "session-a",
        "locator": "artifact:opaque",
        "offset": 0,
        "user_id": "forged-owner"
    }))
    .expect_err("owner selectors are not part of the contract");
    assert!(error.to_string().contains("unknown field"));
}
