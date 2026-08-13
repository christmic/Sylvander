//! UI protocol version and capability negotiation DTOs.
//!
//! Pre-release clients and Runtime agree on one exact schema revision. Unknown
//! revisions fail closed instead of entering an implicit compatibility path.

use serde::{Deserialize, Serialize};

/// The only UI protocol revision accepted by this pre-release build.
///
/// Sylvander intentionally ships one latest schema before its first stable
/// release. Older or newer revisions fail negotiation instead of entering a
/// compatibility path.
pub const UI_PROTOCOL_VERSION: u16 = 6;
pub const UI_PROTOCOL_MIN_VERSION: u16 = UI_PROTOCOL_VERSION;
pub const UI_PROTOCOL_MAX_VERSION: u16 = UI_PROTOCOL_VERSION;
/// Negotiated UI capability for opaque, evidence-backed turn feedback.
pub const FEEDBACK_CAPABILITY: &str = "feedback_v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UiProtocolHello {
    pub client_name: String,
    pub min_version: u16,
    pub max_version: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UiProtocolWelcome {
    pub server_name: String,
    pub version: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UiProtocolError {
    pub code: String,
    pub message: String,
    pub server_min_version: u16,
    pub server_max_version: u16,
}

pub fn negotiate_ui_protocol(hello: &UiProtocolHello) -> Result<u16, UiProtocolError> {
    let selected = hello.max_version.min(UI_PROTOCOL_MAX_VERSION);
    let required_min = hello.min_version.max(UI_PROTOCOL_MIN_VERSION);
    if hello.min_version <= hello.max_version && selected >= required_min {
        return Ok(selected);
    }
    Err(UiProtocolError {
        code: "incompatible_protocol".into(),
        message: format!(
            "client supports {}..={}, server supports {}..={}",
            hello.min_version, hello.max_version, UI_PROTOCOL_MIN_VERSION, UI_PROTOCOL_MAX_VERSION
        ),
        server_min_version: UI_PROTOCOL_MIN_VERSION,
        server_max_version: UI_PROTOCOL_MAX_VERSION,
    })
}

#[cfg(test)]
#[path = "../tests/unit/negotiation.rs"]
mod tests;
