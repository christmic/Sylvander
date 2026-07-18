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
