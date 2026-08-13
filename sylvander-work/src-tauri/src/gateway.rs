//! Authenticated, bounded Runtime transport owned by the native shell.

use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::ipc::Channel;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};

use sylvander_api::{UiClientMessage, UiProtocolHello, UiServerMessage};

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const OUTBOUND_CAPACITY: usize = 128;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum DesktopEvent {
    Connected {
        protocol: sylvander_api::UiProtocolWelcome,
    },
    Message {
        message: Box<UiServerMessage>,
    },
    Disconnected {
        reason: &'static str,
    },
}

struct ActiveConnection {
    generation: u64,
    outbound: mpsc::Sender<UiClientMessage>,
    shutdown: oneshot::Sender<()>,
}

pub(crate) struct DesktopGateway {
    active: Arc<Mutex<Option<ActiveConnection>>>,
    next_generation: AtomicU64,
    config: RuntimeConnectionConfig,
}

struct RuntimeConnectionConfig {
    endpoint: Option<String>,
    bearer: Option<String>,
}

impl Default for DesktopGateway {
    fn default() -> Self {
        Self {
            active: Arc::new(Mutex::new(None)),
            next_generation: AtomicU64::new(1),
            config: RuntimeConnectionConfig {
                endpoint: std::env::var("SYLVANDER_DESKTOP_ENDPOINT").ok(),
                bearer: std::env::var("SYLVANDER_DESKTOP_BEARER").ok(),
            },
        }
    }
}

#[tauri::command]
pub(crate) async fn connect_runtime(
    events: Channel<DesktopEvent>,
    gateway: tauri::State<'_, DesktopGateway>,
) -> Result<(), String> {
    disconnect_active(&gateway).await;
    let endpoint = gateway
        .config
        .endpoint
        .as_deref()
        .ok_or_else(|| "Runtime endpoint is not configured".to_owned())?;
    let request = runtime_request(endpoint, gateway.config.bearer.as_deref())?;
    let config = WebSocketConfig::default()
        .write_buffer_size(32 * 1024)
        .max_write_buffer_size(2 * MAX_MESSAGE_BYTES)
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let (socket, _) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(request, Some(config), true),
    )
    .await
    .map_err(|_| "Runtime connection timed out".to_owned())?
    .map_err(|_| "Runtime connection failed".to_owned())?;

    let (mut write, mut read) = socket.split();
    let hello = UiClientMessage::Hello {
        protocol: UiProtocolHello {
            client_name: "sylvander-work".into(),
            min_version: sylvander_api::UI_PROTOCOL_MIN_VERSION,
            max_version: sylvander_api::UI_PROTOCOL_MAX_VERSION,
            capabilities: desktop_capabilities(),
        },
    };
    write
        .send(encode_message(&hello)?)
        .await
        .map_err(|_| "Runtime handshake failed".to_owned())?;
    let protocol = read_welcome(&mut read).await?;
    events
        .send(DesktopEvent::Connected {
            protocol: protocol.clone(),
        })
        .map_err(|_| "Desktop event channel closed".to_owned())?;

    let (outbound, mut outbound_rx) = mpsc::channel(OUTBOUND_CAPACITY);
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let generation = gateway.next_generation.fetch_add(1, Ordering::Relaxed);
    *gateway.active.lock().await = Some(ActiveConnection {
        generation,
        outbound,
        shutdown,
    });
    let active = gateway.active.clone();

    tauri::async_runtime::spawn(async move {
        let reason = loop {
            tokio::select! {
                command = outbound_rx.recv() => {
                    let Some(command) = command else { break "desktop_closed" };
                    let Ok(message) = encode_message(&command) else { break "invalid_outbound_message" };
                    if write.send(message).await.is_err() { break "send_failed"; }
                }
                inbound = read.next() => {
                    let Some(inbound) = inbound else { break "runtime_closed" };
                    match decode_message(inbound) {
                        Ok(Some(message)) => {
                            if events.send(DesktopEvent::Message { message: Box::new(message) }).is_err() {
                                break "desktop_closed";
                            }
                        }
                        Ok(None) => {}
                        Err(reason) => break reason,
                    }
                }
                _ = &mut shutdown_rx => {
                    let _ = write.send(Message::Close(None)).await;
                    break "requested";
                }
            }
        };
        if finish_current_connection(&active, generation).await {
            let _ = events.send(DesktopEvent::Disconnected { reason });
        }
    });
    Ok(())
}

#[tauri::command]
pub(crate) async fn submit_runtime(
    message: UiClientMessage,
    gateway: tauri::State<'_, DesktopGateway>,
) -> Result<(), String> {
    if matches!(message, UiClientMessage::Hello { .. }) {
        return Err("Protocol negotiation is native-shell owned".into());
    }
    let sender = gateway
        .active
        .lock()
        .await
        .as_ref()
        .map(|active| active.outbound.clone())
        .ok_or_else(|| "Runtime is not connected".to_owned())?;
    sender
        .try_send(message)
        .map_err(|_| "Runtime command queue is unavailable".to_owned())
}

#[tauri::command]
pub(crate) async fn disconnect_runtime(
    gateway: tauri::State<'_, DesktopGateway>,
) -> Result<(), String> {
    disconnect_active(&gateway).await;
    Ok(())
}

async fn disconnect_active(gateway: &DesktopGateway) {
    if let Some(active) = gateway.active.lock().await.take() {
        let _ = active.shutdown.send(());
    }
}

async fn finish_current_connection(
    active: &Mutex<Option<ActiveConnection>>,
    generation: u64,
) -> bool {
    let mut current = active.lock().await;
    if current
        .as_ref()
        .is_some_and(|connection| connection.generation == generation)
    {
        current.take();
        true
    } else {
        false
    }
}

fn runtime_request(
    endpoint: &str,
    bearer: Option<&str>,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, String> {
    let mut request = endpoint
        .into_client_request()
        .map_err(|_| "Runtime endpoint is invalid".to_owned())?;
    let scheme = request.uri().scheme_str();
    if !matches!(scheme, Some("ws" | "wss")) || request.uri().host().is_none() {
        return Err("Runtime endpoint must be an absolute ws or wss URL".into());
    }
    if let Some(value) = bearer {
        if value.is_empty() || value.len() > 8 * 1024 {
            return Err("Runtime bearer lease is invalid".into());
        }
        let value = HeaderValue::from_str(&format!("Bearer {value}"))
            .map_err(|_| "Runtime bearer lease is invalid".to_owned())?;
        request.headers_mut().insert(header::AUTHORIZATION, value);
    }
    Ok(request)
}

fn encode_message(message: &UiClientMessage) -> Result<Message, String> {
    let json =
        serde_json::to_string(message).map_err(|_| "Runtime command encoding failed".to_owned())?;
    if json.len() > MAX_MESSAGE_BYTES {
        return Err("Runtime command exceeds the desktop limit".into());
    }
    Ok(Message::Text(json.into()))
}

fn decode_message(
    message: Result<Message, tokio_tungstenite::tungstenite::Error>,
) -> Result<Option<UiServerMessage>, &'static str> {
    match message.map_err(|_| "receive_failed")? {
        Message::Text(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|_| "invalid_runtime_message"),
        Message::Binary(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| "invalid_runtime_message"),
        Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Ok(None),
        Message::Close(_) => Err("runtime_closed"),
    }
}

async fn read_welcome<S>(read: &mut S) -> Result<sylvander_api::UiProtocolWelcome, String>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let message = tokio::time::timeout(HANDSHAKE_TIMEOUT, read.next())
        .await
        .map_err(|_| "Runtime handshake timed out".to_owned())?
        .ok_or_else(|| "Runtime closed during handshake".to_owned())?;
    match decode_message(message).map_err(str::to_owned)? {
        Some(UiServerMessage::Welcome { protocol })
            if (sylvander_api::UI_PROTOCOL_MIN_VERSION
                ..=sylvander_api::UI_PROTOCOL_MAX_VERSION)
                .contains(&protocol.version) =>
        {
            Ok(protocol)
        }
        Some(UiServerMessage::ProtocolError { error }) => Err(protocol_error_message(&error)),
        _ => Err("Runtime did not acknowledge the UI protocol".into()),
    }
}

fn protocol_error_message(error: &sylvander_api::UiProtocolError) -> String {
    format!(
        "Runtime rejected UI protocol [{}]: {} (server supports {}..={})",
        error.code, error.message, error.server_min_version, error.server_max_version
    )
}

fn desktop_capabilities() -> Vec<String> {
    [
        "attachments",
        "approval_scopes",
        "compaction",
        sylvander_api::FEEDBACK_CAPABILITY,
        sylvander_api::MEMORY_CONFIRMATION_CAPABILITY,
        "model_selection",
        "plans",
        "session_replay",
        "sessions",
        "tasks",
        sylvander_api::USER_PROFILE_CAPABILITY,
        "workspace_rollback",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::{Mutex, mpsc, oneshot};

    use super::{
        ActiveConnection, finish_current_connection, protocol_error_message, runtime_request,
    };

    #[test]
    fn endpoint_requires_websocket_scheme() {
        assert!(runtime_request("https://localhost/ws", None).is_err());
        assert!(runtime_request("ws://127.0.0.1:9000/ws", None).is_ok());
    }

    #[test]
    fn bearer_is_bounded_and_never_returned() {
        assert!(runtime_request("wss://runtime.example/ws", Some("")).is_err());
        let request =
            runtime_request("wss://runtime.example/ws", Some("lease-secret")).expect("valid lease");
        assert_eq!(request.uri().to_string(), "wss://runtime.example/ws");
    }

    #[tokio::test]
    async fn only_the_current_generation_can_publish_disconnect() {
        let (outbound, _) = mpsc::channel(1);
        let (shutdown, _) = oneshot::channel();
        let active = Arc::new(Mutex::new(Some(ActiveConnection {
            generation: 2,
            outbound,
            shutdown,
        })));

        assert!(!finish_current_connection(&active, 1).await);
        assert!(active.lock().await.is_some());
        assert!(finish_current_connection(&active, 2).await);
        assert!(active.lock().await.is_none());
    }

    #[test]
    fn protocol_rejection_preserves_only_the_public_bounded_details() {
        let message = protocol_error_message(&sylvander_api::UiProtocolError {
            code: "incompatible_protocol".into(),
            message: "client and server ranges do not overlap".into(),
            server_min_version: 4,
            server_max_version: 5,
        });

        assert_eq!(
            message,
            "Runtime rejected UI protocol [incompatible_protocol]: client and server ranges do not overlap (server supports 4..=5)"
        );
    }
}
