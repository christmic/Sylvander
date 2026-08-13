use super::*;

#[test]
fn user_id_round_trips() {
    let user: UserId = "alice".into();
    assert_eq!(user.0, "alice");
    let other: UserId = String::from("bob").into();
    assert_eq!(other.0, "bob");
    assert_eq!(user.to_string(), "alice");
}

#[test]
fn user_id_system_sentinel_is_distinct() {
    let system = UserId::system();
    let real = UserId::new("alice");
    assert_ne!(system, real);
    assert_ne!(system.0, "alice");
}

#[test]
fn user_id_serializes_as_inner_string() {
    let user = UserId::new("alice");
    let json = serde_json::to_string(&user).unwrap();
    assert_eq!(json, "\"alice\"");
}

#[test]
fn three_id_types_share_a_constructor_pattern() {
    let _agent: AgentId = "a".into();
    let _instance: AgentInstanceId = "i".into();
    let _session: SessionId = "s".into();
    let _user: UserId = "u".into();
}

#[test]
fn definition_and_instance_identities_remain_distinct() {
    let definition = AgentId::new("researcher");
    let first = AgentInstanceId::new("researcher-1");
    let second = AgentInstanceId::new("researcher-2");

    assert_eq!(definition.to_string(), "researcher");
    assert_ne!(first, second);
    assert_eq!(first.to_string(), "researcher-1");
}
