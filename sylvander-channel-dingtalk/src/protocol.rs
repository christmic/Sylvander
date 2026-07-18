//! `DingTalk` Stream protocol — pure SDK, no Sylvander dependencies.
//!
//! Implements the protocol from `dingtalk-stream-sdk-nodejs`:
//! 1. `POST /gateway/connections/open` → WebSocket endpoint + ticket
//! 2. WebSocket connect → subscribe to robot message topic
//! 3. Receive CALLBACK frames → parse `RobotMessage`
//! 4. Ack messages, send replies via `sessionWebhook` HTTP POST

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sylvander_channel::credential::{
    CredentialLeaseBundle, CredentialLeaseError, CredentialLeaseRequest, CredentialLeaseSource,
};
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

// ===========================================================================
// Constants
// ===========================================================================

/// Stream topic carrying robot callback frames.
pub const ROBOT_TOPIC: &str = "/v1.0/im/bot/messages/get";

const GATEWAY_URL: &str = "https://api.dingtalk.com/v1.0/gateway/connections/open";
const GET_TOKEN_URL: &str = "https://oapi.dingtalk.com/gettoken";

// ===========================================================================
// Gateway types
// ===========================================================================

#[derive(Serialize)]
struct GatewayRequest<'a> {
    #[serde(rename = "clientId")]
    client_id: &'a str,
    #[serde(rename = "clientSecret")]
    client_secret: &'a str,
    subscriptions: Vec<Subscription>,
}

#[derive(Serialize)]
struct Subscription {
    #[serde(rename = "type")]
    sub_type: String,
    topic: String,
}

#[derive(Deserialize)]
struct GatewayResponse {
    endpoint: String,
    ticket: String,
}

// ===========================================================================
// Frame types (WebSocket protocol)
// ===========================================================================

#[derive(Deserialize)]
struct DownStreamFrame {
    #[serde(rename = "type")]
    frame_type: String,
    headers: FrameHeaders,
    data: String,
}

/// Header fields carried by a `DingTalk` Stream frame and acknowledgement.
#[derive(Serialize, Deserialize, Clone)]
pub struct FrameHeaders {
    /// Stable frame identifier echoed by acknowledgements.
    #[serde(rename = "messageId")]
    pub message_id: String,
    /// Stream topic when the frame is a callback.
    pub topic: Option<String>,
    /// Declared frame content type.
    #[serde(rename = "contentType", default)]
    pub content_type: String,
}

#[derive(Serialize)]
struct UpStreamAck {
    code: u16,
    headers: FrameHeaders,
    message: String,
    data: String,
}

// ===========================================================================
// Robot message (parsed from frame.data)
// ===========================================================================

/// Incoming robot message from `DingTalk`.
#[derive(Debug, Clone, Deserialize)]
pub struct RobotMessage {
    /// Stable `DingTalk` conversation identifier.
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    /// Stable robot message identifier used for replay suppression.
    #[serde(rename = "msgId")]
    pub msg_id: String,
    /// Provider sender identifier.
    #[serde(rename = "senderId")]
    pub sender_id: String,
    /// Sender display name; never used for authorization.
    #[serde(rename = "senderNick")]
    pub sender_nick: String,
    /// Enterprise staff identifier used to derive the transport principal.
    #[serde(rename = "senderStaffId")]
    pub sender_staff_id: String,
    /// Per-conversation webhook used for replies.
    #[serde(rename = "sessionWebhook")]
    pub session_webhook: String,
    /// Provider expiry time for the session webhook.
    #[serde(rename = "sessionWebhookExpiredTime")]
    pub session_webhook_expired: i64,
    /// `DingTalk` robot application code.
    #[serde(rename = "robotCode", default)]
    pub robot_code: String,
    /// Provider message kind.
    #[serde(rename = "msgtype")]
    pub msg_type: String,
    /// Plain-text content when `msg_type` is text.
    pub text: Option<TextContent>,
    #[serde(flatten)]
    _extra: JsonValue,
}

/// Plain-text robot message payload.
#[derive(Debug, Clone, Deserialize)]
pub struct TextContent {
    /// User-visible message content.
    pub content: String,
}

// ===========================================================================
// Reply types (HTTP POST to sessionWebhook)
// ===========================================================================

#[derive(Serialize)]
struct WebhookText {
    msgtype: String,
    text: WebhookTextContent,
}

#[derive(Serialize)]
struct WebhookTextContent {
    content: String,
}

#[derive(Serialize)]
struct WebhookMarkdown {
    msgtype: String,
    markdown: WebhookMarkdownContent,
}

#[derive(Serialize)]
struct WebhookMarkdownContent {
    title: String,
    text: String,
}

// ===========================================================================
// Callback trait
// ===========================================================================

/// Handler called when a robot message is received.
#[async_trait::async_trait]
pub trait MessageHandler: Send + Sync {
    /// Report that the Stream WebSocket is established and can receive
    /// callbacks.
    async fn on_connected(&self) {}

    /// Process an incoming robot message. Called in the WebSocket read loop.
    async fn on_message(&self, msg: &RobotMessage, headers: &FrameHeaders);
}

// ===========================================================================
// Client
// ===========================================================================

/// `DingTalk` Stream client — manages WebSocket connection + token + replies.
#[derive(Clone)]
pub struct Client {
    credentials: Arc<dyn CredentialLeaseSource>,
    credential_request: CredentialLeaseRequest,
    http: reqwest::Client,
    token_cache: Arc<Mutex<Option<(String, i64, u64)>>>,
    pub(super) max_message_bytes: usize,
}

impl Client {
    /// Construct a Stream client from one renewable application credential
    /// bundle scoped to `instance_id`.
    pub fn new(
        instance_id: impl Into<String>,
        credentials: Arc<dyn CredentialLeaseSource>,
    ) -> Result<Self, CredentialLeaseError> {
        Ok(Self {
            credential_request: CredentialLeaseRequest::new(
                instance_id,
                ["app_key", "app_secret"],
            )?,
            credentials,
            http: reqwest::Client::new(),
            token_cache: Arc::new(Mutex::new(None)),
            max_message_bytes: 1024 * 1024,
        })
    }

    /// Bound callback frames before JSON deserialization.
    #[must_use]
    pub const fn with_message_limit(mut self, max_message_bytes: usize) -> Self {
        self.max_message_bytes = max_message_bytes;
        self
    }

    /// Connect to `DingTalk` Stream and process messages.
    /// Blocks until the connection is closed.
    pub async fn run(&self, handler: Arc<dyn MessageHandler>) {
        // 1. Get WebSocket endpoint
        let endpoint = match self.get_endpoint().await {
            Ok(ep) => ep,
            Err(e) => {
                tracing::warn!(error = %e, "dingtalk: get_endpoint failed");
                return;
            }
        };

        let ws_url = format!("{}?ticket={}", endpoint.endpoint, endpoint.ticket);
        tracing::info!(%ws_url, "dingtalk: connecting");

        // 2. Connect WebSocket
        let websocket_config = WebSocketConfig::default()
            .max_frame_size(Some(self.max_message_bytes))
            .max_message_size(Some(self.max_message_bytes));
        let (ws, _) = match tokio_tungstenite::connect_async_with_config(
            &ws_url,
            Some(websocket_config),
            false,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "dingtalk: ws connect failed");
                return;
            }
        };
        let (mut write, mut read) = ws.split();
        tracing::info!("dingtalk: connected");
        handler.on_connected().await;

        // 3. Read loop
        while let Some(msg) = read.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    let frame: DownStreamFrame = match serde_json::from_str(&text) {
                        Ok(f) => f,
                        Err(_) => continue,
                    };

                    match frame.frame_type.as_str() {
                        "CALLBACK" => {
                            let robot_msg: RobotMessage = match serde_json::from_str(&frame.data) {
                                Ok(m) => m,
                                Err(e) => {
                                    tracing::warn!(error = %e, "dingtalk: parse failed");
                                    continue;
                                }
                            };

                            handler.on_message(&robot_msg, &frame.headers).await;

                            // Ack
                            let ack = serde_json::to_string(&UpStreamAck {
                                code: 200,
                                headers: frame.headers,
                                message: "OK".into(),
                                data: "{}".into(),
                            })
                            .unwrap();
                            if let Err(error) = write.send(WsMessage::Text(ack.into())).await {
                                tracing::warn!(%error, "dingtalk: acknowledgement failed");
                                break;
                            }
                        }
                        "SYSTEM" => {
                            tracing::debug!(topic = ?frame.headers.topic, "dingtalk: system");
                        }
                        _ => {}
                    }
                }
                Ok(WsMessage::Ping(data)) => {
                    if let Err(error) = write.send(WsMessage::Pong(data)).await {
                        tracing::warn!(%error, "dingtalk: pong failed");
                        break;
                    }
                }
                Ok(WsMessage::Close(_)) => {
                    tracing::info!("dingtalk: closed");
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "dingtalk: ws error");
                    break;
                }
                _ => {}
            }
        }
    }

    /// Reply to a conversation with plain text.
    pub async fn reply_text(&self, webhook_url: &str, text: &str) {
        let Some(token) = self.get_access_token().await else {
            tracing::warn!("dingtalk: credential lease or access token unavailable");
            return;
        };
        self.send_webhook(
            webhook_url,
            &token,
            &WebhookText {
                msgtype: "text".into(),
                text: WebhookTextContent {
                    content: text.to_string(),
                },
            },
        )
        .await;
    }

    /// Reply to a conversation with markdown.
    pub async fn reply_markdown(&self, webhook_url: &str, title: &str, text: &str) {
        let Some(token) = self.get_access_token().await else {
            tracing::warn!("dingtalk: credential lease or access token unavailable");
            return;
        };
        self.send_webhook(
            webhook_url,
            &token,
            &WebhookMarkdown {
                msgtype: "markdown".into(),
                markdown: WebhookMarkdownContent {
                    title: title.to_string(),
                    text: text.to_string(),
                },
            },
        )
        .await;
    }

    // -- internal --

    async fn send_webhook<T: Serialize + ?Sized>(&self, webhook_url: &str, token: &str, body: &T) {
        for attempt in 0..3_u32 {
            match self
                .http
                .post(webhook_url)
                .header("x-acs-dingtalk-access-token", token)
                .json(body)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => return,
                Ok(response)
                    if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS
                        && !response.status().is_server_error() =>
                {
                    tracing::warn!(status = %response.status(), "dingtalk: delivery rejected");
                    return;
                }
                Ok(response) => {
                    tracing::warn!(status = %response.status(), attempt, "dingtalk: delivery retryable");
                }
                Err(error) => {
                    tracing::warn!(%error, attempt, "dingtalk: delivery transport failed");
                }
            }
            if attempt < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(
                    100 * u64::from(attempt + 1),
                ))
                .await;
            }
        }
        tracing::warn!("dingtalk: delivery retry budget exhausted");
    }

    async fn get_endpoint(&self) -> Result<GatewayResponse, CredentialLeaseError> {
        let credentials = self.credential_bundle().await?;
        self.http
            .post(GATEWAY_URL)
            .header("Accept", "application/json")
            .json(&GatewayRequest {
                client_id: credentials.secret("app_key")?,
                client_secret: credentials.secret("app_secret")?,
                subscriptions: vec![Subscription {
                    sub_type: "CALLBACK".into(),
                    topic: ROBOT_TOPIC.into(),
                }],
            })
            .send()
            .await
            .map_err(|_| CredentialLeaseError::Unavailable)?
            .json()
            .await
            .map_err(|_| CredentialLeaseError::Unavailable)
    }

    async fn get_access_token(&self) -> Option<String> {
        let credentials = self.credential_bundle().await.ok()?;
        let credential_generation = credentials.credential_generation();
        if let Some(token) = self
            .cached_access_token(credential_generation, unix_timestamp())
            .await
        {
            return Some(token);
        }

        let url = format!(
            "{}?appkey={}&appsecret={}",
            GET_TOKEN_URL,
            credentials.secret("app_key").ok()?,
            credentials.secret("app_secret").ok()?
        );
        let resp: JsonValue = reqwest::get(&url).await.ok()?.json().await.ok()?;
        let token = resp.get("access_token")?.as_str()?.to_string();
        let expires = unix_timestamp().saturating_add(7000);

        self.token_cache
            .lock()
            .await
            .replace((token.clone(), expires, credential_generation));
        Some(token)
    }

    async fn cached_access_token(&self, credential_generation: u64, now: i64) -> Option<String> {
        let cache = self.token_cache.lock().await;
        let (token, expires, cached_generation) = cache.as_ref()?;
        (now < *expires && *cached_generation == credential_generation).then(|| token.clone())
    }

    async fn credential_bundle(&self) -> Result<CredentialLeaseBundle, CredentialLeaseError> {
        let bundle = self.credentials.lease(&self.credential_request).await?;
        if !bundle.contains_exact_slots(&self.credential_request.slots) {
            return Err(CredentialLeaseError::InvalidLease);
        }
        Ok(bundle)
    }
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
#[path = "../tests/unit/protocol.rs"]
mod tests;
