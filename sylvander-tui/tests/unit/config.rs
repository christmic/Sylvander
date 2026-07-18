use super::*;

#[test]
fn numeric_config_rejects_values_outside_bounds() {
    assert_eq!(parse_number("FPS", None, 30, 5, 120).unwrap(), 30);
    assert_eq!(parse_number("FPS", Some("60"), 30, 5, 120).unwrap(), 60);
    assert!(parse_number("FPS", Some("4"), 30, 5, 120).is_err());
    assert!(parse_number("FPS", Some("fast"), 30, 5, 120).is_err());
}

#[test]
fn accessibility_flags_parse_strictly() {
    assert!(parse_bool("MOTION", Some("yes"), false).unwrap());
    assert!(!parse_bool("ITALIC", Some("0"), true).unwrap());
    assert!(parse_bool("MOTION", Some("sometimes"), false).is_err());
}

#[test]
fn default_theme_name_is_parseable() {
    assert_eq!(
        "sylvander".parse::<ThemeName>().unwrap(),
        ThemeName::Sylvander
    );
}

#[test]
fn report_uses_resolved_values_without_reading_environment_again() {
    let config = TuiConfig {
        socket_path: "/tmp/test.sock".into(),
        initial_session_id: Some("session-1".into()),
        host_bridge: None,
        history_path: None,
        theme: ThemeName::Midnight,
        theme_overrides: ThemeOverrides {
            foreground: Some(ratatui::style::Color::Rgb(0xEE, 0xEE, 0xEE)),
            accent: Some(ratatui::style::Color::Rgb(0xAA, 0x77, 0xFF)),
        },
        color_capability: ColorCapability::TrueColor,
        editing_style: EditingStyle::Vim,
        render_interval: Duration::from_millis(33),
        animation_interval: Duration::from_millis(200),
        reconnect_interval: Duration::from_millis(1_500),
        mouse_scroll_lines: 4,
        reduced_motion: true,
        no_italic: true,
        keymap: KeyMap::default(),
        metadata: RuntimeMetadata::default(),
    };
    let report = config.report(&config.metadata);
    assert!(report.contains("theme       midnight"));
    assert!(report.contains("foreground  #EEEEEE"));
    assert!(report.contains("accent      #AA77FF"));
    assert!(report.contains("colors      truecolor"));
    assert!(report.contains("editing     vim"));
    assert!(report.contains("history     disabled"));
    assert!(report.contains("socket      /tmp/test.sock"));
    assert!(report.contains("session     session-1"));
    assert!(report.contains("sessions=Ctrl+P"));
    assert!(report.contains("animation   reduced"));
    assert!(report.contains("italics     disabled"));
}

#[test]
fn color_capability_prefers_explicit_setting_and_honors_no_color() {
    assert_eq!(
        resolve_color_capability(Some("ansi256"), true, Some("dumb"), None).unwrap(),
        ColorCapability::Ansi256
    );
    assert_eq!(
        resolve_color_capability(None, true, Some("xterm-256color"), None).unwrap(),
        ColorCapability::Monochrome
    );
    assert_eq!(
        resolve_color_capability(Some("AUTO"), false, Some("xterm-256color"), None).unwrap(),
        ColorCapability::Ansi256
    );
    assert!(resolve_color_capability(Some("millions"), false, None, None).is_err());
}

#[test]
fn desktop_launch_options_bind_one_session_and_workspace() {
    let options = parse_launch_options(
        [
            "--socket".into(),
            "/tmp/desktop.sock".into(),
            "--session".into(),
            "session-42".into(),
            "--workspace".into(),
            "/workspace/project".into(),
        ],
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(options.socket_path, PathBuf::from("/tmp/desktop.sock"));
    assert_eq!(options.session_id.as_deref(), Some("session-42"));
    assert_eq!(options.workspace, Some(PathBuf::from("/workspace/project")));
}

#[test]
fn launch_options_reject_removed_positional_socket_syntax() {
    let error = parse_launch_options(
        ["/tmp/legacy.sock".into()],
        Some("/tmp/env.sock".into()),
        Some("session-env".into()),
        None,
    )
    .unwrap_err();
    assert_eq!(error, "unknown argument \"/tmp/legacy.sock\"");
}

#[test]
fn launch_options_reject_ambiguous_or_malformed_input() {
    assert!(
        parse_launch_options(
            ["--socket".into(), "/tmp/a".into(), "/tmp/b".into()],
            None,
            None,
            None,
        )
        .is_err()
    );
    assert!(parse_launch_options(["--session".into()], None, None, None).is_err());
    assert!(parse_launch_options(["--wat".into()], None, None, None).is_err());
}
