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
