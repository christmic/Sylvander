use super::*;

#[test]
fn new_sets_identity_with_defaults() {
    let ctx = SessionContext::new("alice", "code-assistant", "sess-1");
    assert_eq!(ctx.identity.user_id.0, "alice");
    assert_eq!(ctx.identity.agent_id.0, "code-assistant");
    assert_eq!(ctx.identity.session_id.0, "sess-1");
    assert!(ctx.origin.workspace.is_none());
    assert!(ctx.origin.channel.is_none());
    assert_eq!(ctx.request.priority, Priority::Normal);
    assert!(ctx.attributes.is_empty());
}

#[test]
fn system_sentinel_is_distinct() {
    let sys = SessionContext::system();
    let real = SessionContext::new("alice", "a", "s");
    assert_ne!(sys.identity, real.identity);
}

#[test]
fn builder_chain_sets_optional_fields() {
    let ctx = SessionContext::new("alice", "a", "s")
        .with_workspace("/home/alice/code")
        .with_channel("telegram")
        .with_trace_id("req-42")
        .with_priority(Priority::High)
        .with_attribute("experiment", "control")
        .with_attribute("attempt", 3_i64);

    assert_eq!(
        ctx.origin.workspace.as_deref(),
        Some(std::path::Path::new("/home/alice/code"))
    );
    assert_eq!(ctx.origin.channel.as_deref(), Some("telegram"));
    assert_eq!(ctx.request.trace_id.as_deref(), Some("req-42"));
    assert_eq!(ctx.request.priority, Priority::High);
    assert_eq!(ctx.attributes.get_str("experiment"), Some("control"));
    assert_eq!(
        ctx.attributes
            .get("attempt")
            .and_then(AttributeValue::as_i64),
        Some(3)
    );
}

#[test]
fn attribute_bag_overwrites_on_set() {
    let mut bag = AttributeBag::new();
    bag.set("k", "v1");
    let prev = bag.set("k", "v2");
    assert_eq!(prev, Some(AttributeValue::String("v1".into())));
    assert_eq!(bag.get_str("k"), Some("v2"));
}

#[test]
fn attribute_value_accessors_type_check() {
    let v = AttributeValue::String("hi".into());
    assert_eq!(v.as_str(), Some("hi"));
    assert_eq!(v.as_i64(), None);

    let n = AttributeValue::Int(42);
    assert_eq!(n.as_i64(), Some(42));
    assert_eq!(n.as_str(), None);

    let b = AttributeValue::Bool(true);
    assert_eq!(b.as_bool(), Some(true));
    assert_eq!(b.as_str(), None);
}

#[test]
fn attribute_value_serializes_untagged() {
    // `untagged` produces the bare inner value on the wire so
    // downstream consumers can treat it as a JSON string / int / bool.
    let s = serde_json::to_string(&AttributeValue::String("hi".into())).unwrap();
    assert_eq!(s, "\"hi\"");
    let n = serde_json::to_string(&AttributeValue::Int(7)).unwrap();
    assert_eq!(n, "7");
    let b = serde_json::to_string(&AttributeValue::Bool(false)).unwrap();
    assert_eq!(b, "false");
}

#[test]
fn priority_default_is_normal() {
    assert_eq!(Priority::default(), Priority::Normal);
}

#[test]
fn session_context_round_trips_through_json() {
    let original = SessionContext::new("alice", "a", "s")
        .with_workspace("/tmp")
        .with_attribute("k", "v");
    let json = serde_json::to_string(&original).unwrap();
    let restored: SessionContext = serde_json::from_str(&json).unwrap();
    assert_eq!(original, restored);
}
