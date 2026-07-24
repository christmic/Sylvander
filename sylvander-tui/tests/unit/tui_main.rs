use super::*;

#[test]
fn help_describes_the_supported_launch_contract() {
    let output = informational_output(["--help".into()]).unwrap();
    assert!(output.contains("Usage: sylvander-tui [OPTIONS]"));
    assert!(output.contains("--socket <PATH>"));
    assert!(output.contains("--session <ID>"));
    assert!(output.contains("--workspace <PATH>"));
}

#[test]
fn version_uses_the_package_version() {
    assert_eq!(
        informational_output(["-V".into()]).unwrap(),
        format!("sylvander-tui {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn runtime_options_continue_to_the_configuration_parser() {
    assert!(informational_output(["--socket".into(), "/tmp/test.sock".into()]).is_none());
    assert!(informational_output(["--help".into(), "--socket".into()]).is_none());
}
