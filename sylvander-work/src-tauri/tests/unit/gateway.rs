use super::runtime_request;

#[test]
fn endpoint_requires_websocket_scheme() {
    assert!(runtime_request("https://localhost/ws", None).is_err());
    assert!(runtime_request("ws://127.0.0.1:9000/ws", None).is_ok());
}

#[test]
fn bearer_is_bounded_and_never_returned() {
    assert!(runtime_request("wss://runtime.example/ws", Some("")).is_err());
    let request =
        runtime_request("wss://runtime.example/ws", Some("lease-secret")).expect("valid lease");
    assert_eq!(request.uri().to_string(), "wss://runtime.example/ws");
}
