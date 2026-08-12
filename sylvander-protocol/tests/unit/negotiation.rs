use super::*;

#[test]
fn ui_protocol_accepts_only_the_current_revision() {
    let current = UiProtocolHello {
        client_name: "test".into(),
        min_version: UI_PROTOCOL_VERSION,
        max_version: UI_PROTOCOL_VERSION,
        capabilities: vec!["diagnostics".into()],
    };
    assert_eq!(negotiate_ui_protocol(&current), Ok(UI_PROTOCOL_VERSION));

    for version in [UI_PROTOCOL_VERSION - 1, UI_PROTOCOL_VERSION + 1] {
        let incompatible = UiProtocolHello {
            min_version: version,
            max_version: version,
            ..current.clone()
        };
        let error = negotiate_ui_protocol(&incompatible).expect_err("must reject");
        assert_eq!(error.code, "incompatible_protocol");
        assert_eq!(error.server_min_version, UI_PROTOCOL_VERSION);
        assert_eq!(error.server_max_version, UI_PROTOCOL_VERSION);
    }
}
