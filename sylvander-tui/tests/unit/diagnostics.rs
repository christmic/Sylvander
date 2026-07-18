use super::*;

#[test]
fn report_does_not_expose_parent_directories() {
    let mut config = TuiConfig::from_args(std::iter::empty()).expect("config");
    config.socket_path = "/Users/private-name/run/agent.sock".into();
    config.history_path = Some("/Users/private-name/cache/history.json".into());
    let mut state = AppState::new();
    state.metadata.workspace = "/Users/private-name/work/Sylvander".into();
    let report = report(&config, &state);
    assert!(!report.contains("private-name"));
    assert!(report.contains("…/agent.sock"));
    assert!(report.contains("…/Sylvander"));
}

#[test]
fn export_resolves_relative_paths_inside_workspace() {
    let root = tempfile::tempdir().expect("tempdir");
    let target = export("redacted", Path::new("doctor.txt"), root.path()).expect("export");
    assert_eq!(target, root.path().join("doctor.txt"));
    assert_eq!(std::fs::read_to_string(target).expect("read"), "redacted");
    assert!(export("redacted", Path::new("../outside.txt"), root.path()).is_err());
}
