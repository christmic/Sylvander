use super::*;

#[test]
fn runtime_token_rejects_ambiguous_values() {
    assert!(PreparedMutation::from_runtime_token("").is_err());
    assert!(PreparedMutation::from_runtime_token("line\nbreak").is_err());
    assert!(PreparedMutation::from_runtime_token("x".repeat(4_097)).is_err());
}

#[test]
fn runtime_token_round_trips_as_an_opaque_value() {
    let prepared = PreparedMutation::from_runtime_token("runtime-entry-1").expect("valid token");

    assert_eq!(prepared.runtime_token(), "runtime-entry-1");
}
