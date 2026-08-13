//! Versioned, bounded retrieval DTOs for Runtime-owned artifacts.
//!
//! Locators are opaque lookup values, not user authority. Every request also
//! names a Session and Runtime re-derives its owner from the authenticated
//! boundary before reading storage.

use serde::{Deserialize, Serialize};

/// Current version of the artifact range-read contract.
pub const ARTIFACT_READ_PROTOCOL_VERSION: u16 = 1;
/// Largest plaintext range returned by one protocol response.
///
/// Base64 expands this value to exactly 64 KiB, keeping a response bounded
/// independently of the governed record's 16 MiB storage limit.
pub const MAX_ARTIFACT_READ_BYTES: usize = 48 * 1024;

/// Request one bounded byte range from an opaque, Session-bound artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReadRequest {
    pub version: u16,
    pub session_id: String,
    pub locator: String,
    #[serde(default)]
    pub offset: u64,
}

/// One bounded range from a Runtime-authorized artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArtifactChunk {
    pub version: u16,
    pub session_id: String,
    pub locator: String,
    pub media_type: String,
    pub offset: u64,
    pub total_bytes: u64,
    pub next_offset: u64,
    pub eof: bool,
    /// Standard Base64 without line wrapping.
    pub content_base64: String,
    /// SHA-256 digest of the complete plaintext, not merely this range.
    pub payload_digest_sha256: String,
}

#[cfg(test)]
#[path = "../tests/unit/artifact.rs"]
mod tests;
