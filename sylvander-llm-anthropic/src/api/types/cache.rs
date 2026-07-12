//! Prompt cache control.

use serde::{Deserialize, Serialize};

// `Default` is implemented via `#[derive(Default)]` + `#[default]` on the
// `FiveMinutes` variant — clippy flags manual impls.

/// Time-to-live for a prompt cache breakpoint.
///
/// The `5m` variant is the default. `1h` is supported on newer models but
/// has different pricing — see the model registry for per-model support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CacheTtl {
    /// 5 minutes (default).
    #[default]
    #[serde(rename = "5m")]
    FiveMinutes,
    /// 1 hour (newer models only).
    #[serde(rename = "1h")]
    OneHour,
}

/// A cache control breakpoint marker. Attach to any content block to mark
/// the boundary from that block onward as cacheable.
///
/// ```text
/// { "type": "ephemeral", "ttl": "5m" }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheControl {
    /// Always `"ephemeral"` for now.
    #[serde(rename = "type")]
    pub kind: CacheControlKind,
    /// Time-to-live. Omitted on the wire defaults to `5m`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl: Option<CacheTtl>,
}

impl CacheControl {
    /// Create a new ephemeral cache breakpoint with the given TTL.
    #[must_use]
    pub const fn new(ttl: CacheTtl) -> Self {
        Self {
            kind: CacheControlKind::Ephemeral,
            ttl: Some(ttl),
        }
    }

    /// Create a new ephemeral cache breakpoint with the default 5-minute TTL.
    #[must_use]
    pub const fn ephemeral() -> Self {
        Self {
            kind: CacheControlKind::Ephemeral,
            ttl: None,
        }
    }
}

/// Cache control discriminator. Currently only `ephemeral` exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheControlKind {
    /// Short-lived cache, evicted after the configured TTL.
    Ephemeral,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_full() {
        let cc = CacheControl::new(CacheTtl::FiveMinutes);
        assert_eq!(
            serde_json::to_string(&cc).unwrap(),
            r#"{"type":"ephemeral","ttl":"5m"}"#
        );
    }

    #[test]
    fn serializes_without_ttl() {
        let cc = CacheControl::ephemeral();
        assert_eq!(
            serde_json::to_string(&cc).unwrap(),
            r#"{"type":"ephemeral"}"#
        );
    }

    #[test]
    fn serializes_one_hour() {
        let cc = CacheControl::new(CacheTtl::OneHour);
        assert_eq!(
            serde_json::to_string(&cc).unwrap(),
            r#"{"type":"ephemeral","ttl":"1h"}"#
        );
    }

    #[test]
    fn deserializes_round_trip() {
        let json = r#"{"type":"ephemeral","ttl":"5m"}"#;
        let back: CacheControl = serde_json::from_str(json).unwrap();
        assert_eq!(back.ttl, Some(CacheTtl::FiveMinutes));
        assert_eq!(back.kind, CacheControlKind::Ephemeral);
    }

    #[test]
    fn cache_ttl_default_is_five_minutes() {
        assert_eq!(CacheTtl::default(), CacheTtl::FiveMinutes);
    }
}
