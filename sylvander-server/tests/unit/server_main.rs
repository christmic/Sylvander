use super::*;
use std::path::Path;

#[test]
fn server_requires_the_latest_version_configuration_path() {
    assert!(matches!(
        required_config_path(None),
        Err(ServerError::MissingConfig)
    ));
    assert!(matches!(
        required_config_path(Some(OsString::new())),
        Err(ServerError::MissingConfig)
    ));
    assert_eq!(
        required_config_path(Some(OsString::from("/etc/sylvander.toml"))).unwrap(),
        Path::new("/etc/sylvander.toml")
    );
}

#[test]
fn help_names_the_configuration_entry_point() {
    let output = informational_output(["--help".into()]).unwrap().unwrap();
    assert!(output.contains("Usage: sylvander"));
    assert!(output.contains("SYLVANDER_CONFIG"));
    assert!(output.contains("docs/getting-started.md"));
}

#[test]
fn version_uses_the_package_version() {
    assert_eq!(
        informational_output(["--version".into()]).unwrap(),
        Some(format!("sylvander {}\n", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn server_rejects_unknown_arguments_before_reading_configuration() {
    assert!(matches!(
        informational_output(["--wat".into()]),
        Err(ServerError::Cli(message)) if message.contains("--help")
    ));
    assert_eq!(informational_output(Vec::<String>::new()).unwrap(), None);
}
