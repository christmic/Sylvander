use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::config::HostBridgeConfig;

const MAX_RESPONSE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewKind {
    Image,
    Web,
}

#[derive(Serialize)]
struct HostRequest<'a> {
    version: u8,
    session_id: &'a str,
    token: &'a str,
    kind: PreviewKind,
    target: &'a str,
}

#[derive(Deserialize)]
struct HostResponse {
    ok: bool,
    message: String,
}

pub async fn request_preview(
    config: &HostBridgeConfig,
    session_id: &str,
    kind: PreviewKind,
    target: &str,
) -> Result<String, String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        request_preview_inner(config, session_id, kind, target),
    )
    .await
    .map_err(|_| "desktop host request timed out".to_string())?
}

async fn request_preview_inner(
    config: &HostBridgeConfig,
    session_id: &str,
    kind: PreviewKind,
    target: &str,
) -> Result<String, String> {
    let stream = tokio::net::UnixStream::connect(&config.socket_path)
        .await
        .map_err(|error| format!("desktop host unavailable: {error}"))?;
    let (read, mut write) = stream.into_split();
    let request = HostRequest {
        version: 1,
        session_id,
        token: &config.token,
        kind,
        target,
    };
    let encoded = serde_json::to_vec(&request)
        .map_err(|error| format!("could not encode host request: {error}"))?;
    write
        .write_all(&encoded)
        .await
        .map_err(|error| format!("desktop host write failed: {error}"))?;
    write
        .write_all(b"\n")
        .await
        .map_err(|error| format!("desktop host write failed: {error}"))?;
    write
        .shutdown()
        .await
        .map_err(|error| format!("desktop host write shutdown failed: {error}"))?;

    let mut response = Vec::new();
    let mut reader = BufReader::new(read).take(MAX_RESPONSE_BYTES as u64);
    reader
        .read_until(b'\n', &mut response)
        .await
        .map_err(|error| format!("desktop host response failed: {error}"))?;
    if response.last() != Some(&b'\n') {
        return Err("desktop host response exceeded framing limit".into());
    }
    response.pop();
    let response: HostResponse = serde_json::from_slice(&response)
        .map_err(|error| format!("invalid desktop host response: {error}"))?;
    if response.ok {
        Ok(response.message)
    } else {
        Err(response.message)
    }
}

#[cfg(test)]
#[path = "../tests/unit/host_bridge.rs"]
mod tests;
