use super::*;

#[test]
fn credentials_are_not_part_of_the_boundary_contract() {
    let context = BoundaryContext::authenticated(
        AuthenticatedPrincipal::user("alice", AuthenticationMethod::BearerToken),
        "desktop-primary",
        "websocket",
        "request-1",
    );
    let json = serde_json::to_value(context).expect("serialize context");
    assert_eq!(json["principal"]["id"], "alice");
    assert!(json.get("credential").is_none());
    assert!(json["principal"].get("credential").is_none());
}

#[test]
fn unauthenticated_context_is_explicit() {
    let context = BoundaryContext::unauthenticated("terminal", "unix", "request-2");
    assert!(context.principal.is_none());
    assert_eq!(context.channel_instance_id, "terminal");
}

#[test]
fn authentication_failure_cannot_carry_sensitive_content() {
    let failure = AuthenticationFailure::new(AuthenticationMethod::BearerToken);
    let json = serde_json::to_value(failure).unwrap();
    assert_eq!(json["attempted_method"], "bearer_token");
    assert_eq!(json.as_object().unwrap().len(), 1);
    assert_eq!(failure.operation(), "authenticate_bearer_token");
}
