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
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as WsMessage;

// ===========================================================================
// Constants
// ===========================================================================

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

#[derive(Serialize, Deserialize, Clone)]
pub struct FrameHeaders {
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub topic: Option<String>,
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
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    #[serde(rename = "msgId")]
    pub msg_id: String,
    #[serde(rename = "senderId")]
    pub sender_id: String,
    #[serde(rename = "senderNick")]
    pub sender_nick: String,
    #[serde(rename = "senderStaffId")]
    pub sender_staff_id: String,
    #[serde(rename = "sessionWebhook")]
    pub session_webhook: String,
    #[serde(rename = "sessionWebhookExpiredTime")]
    pub session_webhook_expired: i64,
    #[serde(rename = "robotCode", default)]
    pub robot_code: String,
    #[serde(rename = "msgtype")]
    pub msg_type: String,
    pub text: Option<TextContent>,
    #[serde(flatten)]
    _extra: JsonValue,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TextContent {
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
    /// Process an incoming robot message. Called in the WebSocket read loop.
    async fn on_message(&self, msg: &RobotMessage, headers: &FrameHeaders);
}

// ===========================================================================
// Client
// ===========================================================================

/// `DingTalk` Stream client — manages WebSocket connection + token + replies.
#[derive(Clone)]
pub struct Client {
    app_key: String,
    app_secret: String,
    http: reqwest::Client,
    token_cache: Arc<Mutex<Option<(String, i64)>>>,
}

impl Client {
    pub fn new(app_key: impl Into<String>, app_secret: impl Into<String>) -> Self {
        Self {
            app_key: app_key.into(),
            app_secret: app_secret.into(),
            http: reqwest::Client::new(),
            token_cache: Arc::new(Mutex::new(None)),
        }
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
        let (ws, _) = match tokio_tungstenite::connect_async(&ws_url).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "dingtalk: ws connect failed");
                return;
            }
        };
        let (mut write, mut read) = ws.split();
        tracing::info!("dingtalk: connected");

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
                            let _ = write.send(WsMessage::Text(ack.into())).await;
                        }
                        "SYSTEM" => {
                            tracing::debug!(topic = ?frame.headers.topic, "dingtalk: system");
                        }
                        _ => {}
                    }
                }
                Ok(WsMessage::Ping(data)) => {
                    let _ = write.send(WsMessage::Pong(data)).await;
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
        let token = self.get_access_token().await;
        let _ = self
            .http
            .post(webhook_url)
            .header(
                "x-acs-dingtalk-access-token",
                token.as_deref().unwrap_or(""),
            )
            .json(&WebhookText {
                msgtype: "text".into(),
                text: WebhookTextContent {
                    content: text.to_string(),
                },
            })
            .send()
            .await;
    }

    /// Reply to a conversation with markdown.
    pub async fn reply_markdown(&self, webhook_url: &str, title: &str, text: &str) {
        let token = self.get_access_token().await;
        let _ = self
            .http
            .post(webhook_url)
            .header(
                "x-acs-dingtalk-access-token",
                token.as_deref().unwrap_or(""),
            )
            .json(&WebhookMarkdown {
                msgtype: "markdown".into(),
                markdown: WebhookMarkdownContent {
                    title: title.to_string(),
                    text: text.to_string(),
                },
            })
            .send()
            .await;
    }

    // -- internal --

    async fn get_endpoint(&self) -> Result<GatewayResponse, reqwest::Error> {
        self.http
            .post(GATEWAY_URL)
            .header("Accept", "application/json")
            .json(&GatewayRequest {
                client_id: &self.app_key,
                client_secret: &self.app_secret,
                subscriptions: vec![Subscription {
                    sub_type: "CALLBACK".into(),
                    topic: ROBOT_TOPIC.into(),
                }],
            })
            .send()
            .await?
            .json()
            .await
    }

    async fn get_access_token(&self) -> Option<String> {
        {
            let cache = self.token_cache.lock().await;
            if let Some((token, expires)) = &*cache {
                let now = unix_timestamp();
                if now < *expires {
                    return Some(token.clone());
                }
            }
        }

        let url = format!(
            "{}?appkey={}&appsecret={}",
            GET_TOKEN_URL, self.app_key, self.app_secret
        );
        let resp: JsonValue = reqwest::get(&url).await.ok()?.json().await.ok()?;
        let token = resp.get("access_token")?.as_str()?.to_string();
        let expires = unix_timestamp().saturating_add(7000);

        self.token_cache
            .lock()
            .await
            .replace((token.clone(), expires));
        Some(token)
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
