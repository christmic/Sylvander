use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::Url;
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, ETAG, HeaderValue, IF_MATCH, IF_NONE_MATCH,
};
use sha2::{Digest, Sha256};

pub(super) const MAX_ANCHOR_BYTES: u64 = 8 * 1024;
const MAX_REVISION_BYTES: usize = 512;
const MIN_HTTP_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_READ_RETRIES: u8 = 3;
const MAX_HTTP_SECRET_BYTES: usize = 64 * 1024;

/// Opaque compare-and-swap revision issued by an integrity-anchor backend.
#[derive(Clone, PartialEq, Eq)]
pub struct MemoryAnchorRevision(String);

impl fmt::Debug for MemoryAnchorRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MemoryAnchorRevision([redacted])")
    }
}

impl MemoryAnchorRevision {
    pub(super) fn new(value: String) -> Result<Self, MemoryAnchorError> {
        if value.is_empty() || value.len() > MAX_REVISION_BYTES || value.contains(['\r', '\n']) {
            return Err(MemoryAnchorError::InvalidResponse);
        }
        Ok(Self(value))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

/// One authenticated anchor value and the opaque revision needed to replace it.
pub struct MemoryAnchorObservation {
    pub revision: MemoryAnchorRevision,
    pub value: Vec<u8>,
}

impl fmt::Debug for MemoryAnchorObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryAnchorObservation")
            .field("revision", &self.revision)
            .field("value", &"[redacted]")
            .finish()
    }
}

/// Content-safe anchor failure. Implementations must never include credentials,
/// endpoints, response bodies, or stored anchor values in this error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MemoryAnchorError {
    #[error("memory anchor is unavailable")]
    Unavailable,
    #[error("memory anchor compare-and-swap conflict")]
    Conflict,
    #[error("memory anchor returned an invalid response")]
    InvalidResponse,
}

/// Backend-neutral monotonic compare-and-swap boundary for memory integrity.
///
/// `create` must be create-if-absent. `compare_and_swap` must replace a value
/// only when `expected` still names the current backend revision. Backends must
/// never silently downgrade to another implementation when an operation fails.
pub trait MonotonicMemoryAnchor: Send + Sync {
    fn load(&self) -> Result<Option<MemoryAnchorObservation>, MemoryAnchorError>;

    fn create(&self, value: &[u8]) -> Result<MemoryAnchorRevision, MemoryAnchorError>;

    fn compare_and_swap(
        &self,
        expected: &MemoryAnchorRevision,
        value: &[u8],
    ) -> Result<MemoryAnchorRevision, MemoryAnchorError>;
}

/// Host-file anchor. This backend relies on deployment permissions and does
/// not protect against an administrator replaying the file with the database.
#[derive(Debug, Clone)]
pub struct FileMemoryIntegrityAnchor {
    path: PathBuf,
}

impl FileMemoryIntegrityAnchor {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn read_value(&self) -> Result<Vec<u8>, MemoryAnchorError> {
        let metadata = std::fs::metadata(&self.path).map_err(|_| MemoryAnchorError::Unavailable)?;
        if !metadata.is_file() || metadata.len() > MAX_ANCHOR_BYTES {
            return Err(MemoryAnchorError::InvalidResponse);
        }
        std::fs::read(&self.path).map_err(|_| MemoryAnchorError::Unavailable)
    }

    fn write_value(&self, value: &[u8], create_new: bool) -> Result<(), MemoryAnchorError> {
        if value.len() as u64 > MAX_ANCHOR_BYTES {
            return Err(MemoryAnchorError::InvalidResponse);
        }
        let parent = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or(MemoryAnchorError::Unavailable)?;
        std::fs::create_dir_all(parent).map_err(|_| MemoryAnchorError::Unavailable)?;
        let temp = parent.join(format!(".memory-anchor-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|_| MemoryAnchorError::Unavailable)?;
            file.write_all(value)
                .map_err(|_| MemoryAnchorError::Unavailable)?;
            file.sync_all()
                .map_err(|_| MemoryAnchorError::Unavailable)?;
            secure_file(&temp)?;
            if create_new {
                std::fs::hard_link(&temp, &self.path).map_err(|_| MemoryAnchorError::Conflict)?;
                std::fs::remove_file(&temp).map_err(|_| MemoryAnchorError::Unavailable)?;
            } else {
                std::fs::rename(&temp, &self.path).map_err(|_| MemoryAnchorError::Unavailable)?;
            }
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| MemoryAnchorError::Unavailable)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
}

impl MonotonicMemoryAnchor for FileMemoryIntegrityAnchor {
    fn load(&self) -> Result<Option<MemoryAnchorObservation>, MemoryAnchorError> {
        match self.read_value() {
            Ok(value) => Ok(Some(MemoryAnchorObservation {
                revision: file_revision(&value),
                value,
            })),
            Err(MemoryAnchorError::Unavailable) if !self.path.exists() => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn create(&self, value: &[u8]) -> Result<MemoryAnchorRevision, MemoryAnchorError> {
        self.write_value(value, true)?;
        Ok(file_revision(value))
    }

    fn compare_and_swap(
        &self,
        expected: &MemoryAnchorRevision,
        value: &[u8],
    ) -> Result<MemoryAnchorRevision, MemoryAnchorError> {
        let current = self.read_value()?;
        if file_revision(&current) != *expected {
            return Err(MemoryAnchorError::Conflict);
        }
        self.write_value(value, false)?;
        Ok(file_revision(value))
    }
}

/// Configuration for the remote strong-consistency HTTP CAS backend.
///
/// The service must expose one resource: `GET` returns the current value and a
/// strong `ETag`; `PUT` with `If-None-Match: *` creates it; and `PUT` with
/// `If-Match: <etag>` atomically replaces it. `409` and `412` are CAS conflicts.
pub struct HttpMemoryIntegrityAnchorConfig {
    endpoint: Url,
    bearer_token: Vec<u8>,
    timeout: Duration,
    read_retries: u8,
    ca_certificate_pem: Option<Vec<u8>>,
    client_identity_pem: Option<Vec<u8>>,
    allow_http: bool,
}

impl fmt::Debug for HttpMemoryIntegrityAnchorConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpMemoryIntegrityAnchorConfig")
            .field("endpoint", &"[redacted]")
            .field("bearer_token", &"[redacted]")
            .field("timeout", &self.timeout)
            .field("read_retries", &self.read_retries)
            .field("ca_certificate_pem", &self.ca_certificate_pem.is_some())
            .field("client_identity_pem", &self.client_identity_pem.is_some())
            .finish()
    }
}

impl Drop for HttpMemoryIntegrityAnchorConfig {
    fn drop(&mut self) {
        self.bearer_token.fill(0);
        if let Some(value) = &mut self.ca_certificate_pem {
            value.fill(0);
        }
        if let Some(value) = &mut self.client_identity_pem {
            value.fill(0);
        }
    }
}

impl HttpMemoryIntegrityAnchorConfig {
    pub fn new(
        endpoint: &str,
        bearer_token: &[u8],
        timeout: Duration,
        read_retries: u8,
    ) -> Result<Self, MemoryAnchorError> {
        Self::parse(endpoint, bearer_token, timeout, read_retries, false)
    }

    #[cfg(test)]
    pub(crate) fn new_test_http(
        endpoint: &str,
        bearer_token: &[u8],
        timeout: Duration,
        read_retries: u8,
    ) -> Result<Self, MemoryAnchorError> {
        Self::parse(endpoint, bearer_token, timeout, read_retries, true)
    }

    fn parse(
        endpoint: &str,
        bearer_token: &[u8],
        timeout: Duration,
        read_retries: u8,
        allow_http: bool,
    ) -> Result<Self, MemoryAnchorError> {
        let endpoint = Url::parse(endpoint).map_err(|_| MemoryAnchorError::InvalidResponse)?;
        if (!allow_http && endpoint.scheme() != "https")
            || (allow_http && !matches!(endpoint.scheme(), "http" | "https"))
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !(MIN_HTTP_TIMEOUT..=MAX_HTTP_TIMEOUT).contains(&timeout)
            || read_retries > MAX_READ_RETRIES
            || bearer_token.is_empty()
            || bearer_token.len() > MAX_HTTP_SECRET_BYTES
            || std::str::from_utf8(bearer_token).is_err()
        {
            return Err(MemoryAnchorError::InvalidResponse);
        }
        let authorization = format!(
            "Bearer {}",
            std::str::from_utf8(bearer_token).map_err(|_| MemoryAnchorError::InvalidResponse)?
        );
        HeaderValue::from_str(&authorization).map_err(|_| MemoryAnchorError::InvalidResponse)?;
        Ok(Self {
            endpoint,
            bearer_token: bearer_token.to_vec(),
            timeout,
            read_retries,
            ca_certificate_pem: None,
            client_identity_pem: None,
            allow_http,
        })
    }

    pub fn with_ca_certificate(mut self, pem: &[u8]) -> Result<Self, MemoryAnchorError> {
        validate_pem_size(pem)?;
        self.ca_certificate_pem = Some(pem.to_vec());
        Ok(self)
    }

    pub fn with_client_identity(mut self, pem: &[u8]) -> Result<Self, MemoryAnchorError> {
        validate_pem_size(pem)?;
        self.client_identity_pem = Some(pem.to_vec());
        Ok(self)
    }
}

/// Remote CAS anchor. Unlike the file backend, its monotonic revision and
/// value live outside the database host's replay boundary.
pub struct HttpMemoryIntegrityAnchor {
    client: Client,
    endpoint: Url,
    bearer_token: Mutex<Vec<u8>>,
    read_retries: u8,
}

impl fmt::Debug for HttpMemoryIntegrityAnchor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpMemoryIntegrityAnchor")
            .field("endpoint", &"[redacted]")
            .field("bearer_token", &"[redacted]")
            .field("read_retries", &self.read_retries)
            .finish_non_exhaustive()
    }
}

impl Drop for HttpMemoryIntegrityAnchor {
    fn drop(&mut self) {
        if let Ok(token) = self.bearer_token.get_mut() {
            token.fill(0);
        }
    }
}

impl HttpMemoryIntegrityAnchor {
    pub fn new(mut config: HttpMemoryIntegrityAnchorConfig) -> Result<Self, MemoryAnchorError> {
        let mut builder = Client::builder().timeout(config.timeout);
        if !config.allow_http {
            builder = builder.https_only(true);
        }
        if let Some(pem) = &config.ca_certificate_pem {
            let certificate = reqwest::Certificate::from_pem(pem)
                .map_err(|_| MemoryAnchorError::InvalidResponse)?;
            builder = builder.add_root_certificate(certificate);
        }
        if let Some(pem) = &config.client_identity_pem {
            let identity =
                reqwest::Identity::from_pem(pem).map_err(|_| MemoryAnchorError::InvalidResponse)?;
            builder = builder.identity(identity);
        }
        let bearer_token = std::mem::take(&mut config.bearer_token);
        Ok(Self {
            client: builder
                .build()
                .map_err(|_| MemoryAnchorError::InvalidResponse)?,
            endpoint: config.endpoint.clone(),
            bearer_token: Mutex::new(bearer_token),
            read_retries: config.read_retries,
        })
    }

    fn authorization(&self) -> Result<HeaderValue, MemoryAnchorError> {
        let token = self
            .bearer_token
            .lock()
            .map_err(|_| MemoryAnchorError::Unavailable)?;
        let token = std::str::from_utf8(&token).map_err(|_| MemoryAnchorError::Unavailable)?;
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| MemoryAnchorError::Unavailable)
    }

    fn load_once(&self) -> Result<Option<MemoryAnchorObservation>, MemoryAnchorError> {
        let response = self
            .client
            .get(self.endpoint.clone())
            .header(AUTHORIZATION, self.authorization()?)
            .send()
            .map_err(|_| MemoryAnchorError::Unavailable)?;
        match response.status().as_u16() {
            200 => {
                let revision = response_revision(&response)?;
                let value = read_bounded(response)?;
                Ok(Some(MemoryAnchorObservation { revision, value }))
            }
            404 => Ok(None),
            408 | 429 | 500..=599 => Err(MemoryAnchorError::Unavailable),
            _ => Err(MemoryAnchorError::InvalidResponse),
        }
    }

    fn put(
        &self,
        condition: (reqwest::header::HeaderName, HeaderValue),
        value: &[u8],
    ) -> Result<MemoryAnchorRevision, MemoryAnchorError> {
        if value.len() as u64 > MAX_ANCHOR_BYTES {
            return Err(MemoryAnchorError::InvalidResponse);
        }
        let response = self
            .client
            .put(self.endpoint.clone())
            .header(AUTHORIZATION, self.authorization()?)
            .header(CONTENT_TYPE, "application/json")
            .header(condition.0, condition.1)
            .body(value.to_vec())
            .send()
            .map_err(|_| MemoryAnchorError::Unavailable)?;
        match response.status().as_u16() {
            200 | 201 => response_revision(&response),
            409 | 412 => Err(MemoryAnchorError::Conflict),
            408 | 429 | 500..=599 => Err(MemoryAnchorError::Unavailable),
            _ => Err(MemoryAnchorError::InvalidResponse),
        }
    }
}

impl MonotonicMemoryAnchor for HttpMemoryIntegrityAnchor {
    fn load(&self) -> Result<Option<MemoryAnchorObservation>, MemoryAnchorError> {
        for attempt in 0..=self.read_retries {
            match self.load_once() {
                Err(MemoryAnchorError::Unavailable) if attempt < self.read_retries => {
                    std::thread::sleep(Duration::from_millis(20 * u64::from(attempt + 1)));
                }
                result => return result,
            }
        }
        Err(MemoryAnchorError::Unavailable)
    }

    fn create(&self, value: &[u8]) -> Result<MemoryAnchorRevision, MemoryAnchorError> {
        self.put((IF_NONE_MATCH, HeaderValue::from_static("*")), value)
    }

    fn compare_and_swap(
        &self,
        expected: &MemoryAnchorRevision,
        value: &[u8],
    ) -> Result<MemoryAnchorRevision, MemoryAnchorError> {
        let expected = HeaderValue::from_str(expected.as_str())
            .map_err(|_| MemoryAnchorError::InvalidResponse)?;
        self.put((IF_MATCH, expected), value)
    }
}

fn response_revision(response: &Response) -> Result<MemoryAnchorRevision, MemoryAnchorError> {
    let value = response
        .headers()
        .get(ETAG)
        .ok_or(MemoryAnchorError::InvalidResponse)?
        .to_str()
        .map_err(|_| MemoryAnchorError::InvalidResponse)?;
    if value.starts_with("W/") || !value.starts_with('"') || !value.ends_with('"') {
        return Err(MemoryAnchorError::InvalidResponse);
    }
    MemoryAnchorRevision::new(value.to_owned())
}

fn read_bounded(mut response: Response) -> Result<Vec<u8>, MemoryAnchorError> {
    if response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_ANCHOR_BYTES)
    {
        return Err(MemoryAnchorError::InvalidResponse);
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut response)
        .take(MAX_ANCHOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| MemoryAnchorError::Unavailable)?;
    if bytes.len() as u64 > MAX_ANCHOR_BYTES {
        return Err(MemoryAnchorError::InvalidResponse);
    }
    Ok(bytes)
}

fn validate_pem_size(pem: &[u8]) -> Result<(), MemoryAnchorError> {
    if pem.is_empty() || pem.len() > MAX_HTTP_SECRET_BYTES {
        return Err(MemoryAnchorError::InvalidResponse);
    }
    Ok(())
}

fn file_revision(value: &[u8]) -> MemoryAnchorRevision {
    MemoryAnchorRevision(format!("sha256:{:x}", Sha256::digest(value)))
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<(), MemoryAnchorError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| MemoryAnchorError::Unavailable)
}

#[cfg(not(unix))]
fn secure_file(_: &Path) -> Result<(), MemoryAnchorError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_backend_enforces_create_and_compare_and_swap() {
        let directory = tempfile::tempdir().unwrap();
        let anchor = FileMemoryIntegrityAnchor::new(directory.path().join("anchor.json"));
        assert!(anchor.load().unwrap().is_none());
        let first = anchor.create(b"first").unwrap();
        assert_eq!(anchor.load().unwrap().unwrap().value, b"first");
        assert_eq!(
            anchor.create(b"duplicate"),
            Err(MemoryAnchorError::Conflict)
        );
        let second = anchor.compare_and_swap(&first, b"second").unwrap();
        assert_eq!(anchor.load().unwrap().unwrap().value, b"second");
        assert_eq!(
            anchor.compare_and_swap(&first, b"stale"),
            Err(MemoryAnchorError::Conflict)
        );
        assert_ne!(first, second);
    }

    #[test]
    fn revisions_do_not_disclose_the_stored_value() {
        let revision = file_revision(b"private anchor value");
        assert_eq!(format!("{revision:?}"), "MemoryAnchorRevision([redacted])");
        assert!(!revision.as_str().contains("private"));
    }
}
