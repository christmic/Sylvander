use std::fmt;
use std::io::Read as _;

use super::SecretRef;

const MAX_SECRET_BYTES: u64 = 64 * 1024;

/// A resolved secret that redacts formatting and clears its owned bytes when
/// dropped. Callers should keep this value short-lived.
pub struct SecretValue(Vec<u8>);

impl SecretValue {
    fn new(bytes: Vec<u8>) -> Result<Self, SecretResolveError> {
        if bytes.is_empty() {
            return Err(SecretResolveError::Empty);
        }
        Ok(Self(bytes))
    }

    pub fn as_str(&self) -> Result<&str, SecretResolveError> {
        std::str::from_utf8(&self.0).map_err(|_| SecretResolveError::NotUtf8)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretValue {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

impl fmt::Display for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretResolveError {
    #[error("secret environment variable `{name}` is unavailable")]
    MissingEnvironment { name: String },
    #[error("failed to inspect secret file {path}: {message}")]
    InspectFile { path: String, message: String },
    #[error("secret file {path} is not a regular file")]
    NotRegularFile { path: String },
    #[error("secret file {path} exceeds {limit} byte limit")]
    TooLarge { path: String, limit: u64 },
    #[error("failed to read secret file {path}: {message}")]
    ReadFile { path: String, message: String },
    #[error("resolved secret is empty")]
    Empty,
    #[error("resolved secret is not UTF-8")]
    NotUtf8,
}

/// Resolves configured secret references at the infrastructure boundary.
pub trait SecretResolver: Send + Sync {
    fn resolve(&self, reference: &SecretRef) -> Result<SecretValue, SecretResolveError>;
}

/// Reads secrets from the server process environment or a bounded regular
/// file. Secret values never become part of [`super::ServerConfig`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSecretResolver;

impl SecretResolver for SystemSecretResolver {
    fn resolve(&self, reference: &SecretRef) -> Result<SecretValue, SecretResolveError> {
        match reference {
            SecretRef::Env { name } => {
                let value = std::env::var_os(name)
                    .ok_or_else(|| SecretResolveError::MissingEnvironment { name: name.clone() })?;
                SecretValue::new(value.to_string_lossy().into_owned().into_bytes())
            }
            SecretRef::File { path } => {
                let display = path.display().to_string();
                let metadata =
                    std::fs::metadata(path).map_err(|error| SecretResolveError::InspectFile {
                        path: display.clone(),
                        message: error.to_string(),
                    })?;
                if !metadata.is_file() {
                    return Err(SecretResolveError::NotRegularFile { path: display });
                }
                if metadata.len() > MAX_SECRET_BYTES {
                    return Err(SecretResolveError::TooLarge {
                        path: display,
                        limit: MAX_SECRET_BYTES,
                    });
                }
                let file =
                    std::fs::File::open(path).map_err(|error| SecretResolveError::ReadFile {
                        path: display.clone(),
                        message: error.to_string(),
                    })?;
                let mut bytes = Vec::with_capacity(metadata.len() as usize);
                file.take(MAX_SECRET_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| SecretResolveError::ReadFile {
                        path: display.clone(),
                        message: error.to_string(),
                    })?;
                if bytes.len() as u64 > MAX_SECRET_BYTES {
                    return Err(SecretResolveError::TooLarge {
                        path: display,
                        limit: MAX_SECRET_BYTES,
                    });
                }
                while matches!(bytes.last(), Some(b'\n' | b'\r')) {
                    bytes.pop();
                }
                SecretValue::new(bytes)
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
}
