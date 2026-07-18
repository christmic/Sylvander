use super::*;

#[test]
fn file_secret_is_bounded_trimmed_and_redacted() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("secret");
    std::fs::write(&path, "sensitive-value\r\n").unwrap();
    let value = SystemSecretResolver
        .resolve(&SecretRef::File { path })
        .unwrap();
    assert_eq!(value.as_str().unwrap(), "sensitive-value");
    assert_eq!(format!("{value:?}"), "SecretValue([REDACTED])");
    assert_eq!(value.to_string(), "[REDACTED]");
}

#[test]
fn missing_environment_secret_never_echoes_a_value() {
    let name = format!("SYLVANDER_MISSING_SECRET_{}", uuid::Uuid::new_v4());
    let error = SystemSecretResolver
        .resolve(&SecretRef::Env { name: name.clone() })
        .unwrap_err();
    assert!(error.to_string().contains(&name));
    assert!(!error.to_string().contains("sensitive-value"));
}

#[test]
fn directory_and_empty_file_are_rejected() {
    let directory = tempfile::TempDir::new().unwrap();
    assert!(matches!(
        SystemSecretResolver.resolve(&SecretRef::File {
            path: directory.path().into()
        }),
        Err(SecretResolveError::NotRegularFile { .. })
    ));
    let path = directory.path().join("empty");
    std::fs::write(&path, "\n").unwrap();
    assert!(matches!(
        SystemSecretResolver.resolve(&SecretRef::File { path }),
        Err(SecretResolveError::Empty)
    ));
}
