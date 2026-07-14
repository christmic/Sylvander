//! `WeChat` enterprise bot channel — encrypted XML callbacks.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::Deserialize;
use tracing::{info, warn};

use sylvander_agent::bus::{BusMessage, MessageKind, StreamEvent, SubscriptionFilter};
use sylvander_agent::session_store::SessionStore;
use sylvander_agent::spec::{AgentId, SessionId};
use sylvander_channel::{Channel, ChannelContext, ExternalChatRequest, authorize_external_chat};
use sylvander_protocol::{AuthenticatedPrincipal, AuthenticationMethod, BoundaryContext};

use protocol::{WechatCrypto, parse_message_xml};

pub mod protocol;

// ===========================================================================
// Channel
// ===========================================================================

pub struct WechatChannel {
    crypto: Arc<WechatCrypto>,
    webhook_addr: SocketAddr,
    agent_id: AgentId,
    instance_id: String,
}

impl WechatChannel {
    pub fn new(
        token: String,
        encoding_aes_key: String,
        corp_id: String,
        webhook_addr: SocketAddr,
        agent_id: impl Into<AgentId>,
    ) -> Result<Self, String> {
        let crypto = WechatCrypto::new(token, &encoding_aes_key, corp_id)
            .map_err(|e| format!("wechat crypto init: {e}"))?;
        Ok(Self {
            crypto: Arc::new(crypto),
            webhook_addr,
            agent_id: agent_id.into(),
            instance_id: "wechat".into(),
        })
    }

    /// Identify this configured application for session and principal isolation.
    #[must_use]
    pub fn with_instance_id(mut self, instance_id: impl Into<String>) -> Self {
        self.instance_id = instance_id.into();
        self
    }
}

#[async_trait]
impl Channel for WechatChannel {
    fn name(&self) -> &'static str {
        "wechat"
    }

    async fn run(self: Arc<Self>, ctx: ChannelContext) {
        let ctx = Arc::new(ctx);

        // Outgoing loop: bus → encrypted XML reply
        let ch = self.clone();
        let ctx_out = ctx.clone();
        let outgoing = tokio::spawn(async move { run_outgoing(ch.clone(), ctx_out).await });

        // HTTP server
        let state = Arc::new(AppState {
            sessions: ctx.sessions.clone(),
            ctx,
            crypto: self.crypto.clone(),
            agent_id: self.agent_id.clone(),
            instance_id: self.instance_id.clone(),
        });

        let app = Router::new()
            .route("/wechat/callback", get(handle_verify).post(handle_callback))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind(self.webhook_addr)
            .await
            .unwrap();
        info!(addr = %self.webhook_addr, "wechat channel listening");
        state.ctx.mark_ready();
        let shutdown = state.ctx.clone();
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { shutdown.shutdown_requested().await })
            .await
            .unwrap();
        outgoing.abort();
        let _ = outgoing.await;
    }
}

impl Clone for WechatChannel {
    fn clone(&self) -> Self {
        Self {
            crypto: self.crypto.clone(),
            webhook_addr: self.webhook_addr,
            agent_id: self.agent_id.clone(),
            instance_id: self.instance_id.clone(),
        }
    }
}

// ===========================================================================
// App state
// ===========================================================================

struct AppState {
    ctx: Arc<ChannelContext>,
    crypto: Arc<WechatCrypto>,
    agent_id: AgentId,
    sessions: Arc<dyn SessionStore>,
    instance_id: String,
}

// ===========================================================================
// URL verification (GET)
// ===========================================================================

#[derive(Deserialize)]
struct CallbackQuery {
    msg_signature: String,
    timestamp: String,
    nonce: String,
    echostr: Option<String>,
}

async fn handle_verify(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CallbackQuery>,
) -> String {
    // Verify signature then decrypt echostr
    let echostr = q.echostr.unwrap_or_default();
    if !state
        .crypto
        .verify_signature(&q.msg_signature, &q.timestamp, &q.nonce, &echostr)
    {
        return String::new();
    }
    match state.crypto.decrypt(&echostr) {
        Ok((msg, _)) => msg,
        Err(_) => String::new(),
    }
}

// ===========================================================================
// Incoming message (POST)
// ===========================================================================

#[derive(Deserialize)]
struct CallbackBody {
    encrypt: Option<String>,
}

async fn handle_callback(
    State(state): State<Arc<AppState>>,
    Query(q): Query<CallbackQuery>,
    Json(body): Json<CallbackBody>,
) -> String {
    let Some(encrypted) = body.encrypt else {
        return "success".into();
    };

    if !state
        .crypto
        .verify_signature(&q.msg_signature, &q.timestamp, &q.nonce, &encrypted)
    {
        warn!("wechat: signature invalid");
        return "success".into();
    }

    let (xml, _corp_id) = match state.crypto.decrypt(&encrypted) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "wechat: decrypt failed");
            return "success".into();
        }
    };

    let msg = match parse_message_xml(&xml) {
        Ok(m) => m,
        Err(e) => {
            warn!(error = %e, xml = %xml, "wechat: xml parse failed");
            return "success".into();
        }
    };

    info!(from = %msg.from_user_name, msg_type = %msg.msg_type, "wechat: message");

    // Session mapping
    let existing = find_by_user(&state.sessions, &state.instance_id, &msg.from_user_name).await;
    let boundary = BoundaryContext::authenticated(
        AuthenticatedPrincipal::user(
            format!("wechat:{}:{}", state.instance_id, msg.from_user_name),
            AuthenticationMethod::PlatformIdentity,
        ),
        &state.instance_id,
        "wechat",
        format!("wechat-message-{}", msg.msg_id),
    );
    let external_meta = BTreeMap::from([
        ("channel_instance_id".into(), state.instance_id.clone()),
        ("from_user_name".into(), msg.from_user_name.clone()),
    ]);
    let session_id = match authorize_external_chat(
        &state.ctx,
        &boundary,
        ExternalChatRequest {
            existing_session: existing,
            agent_id: state.agent_id.clone(),
            label: format!("wechat-{}", msg.from_user_name),
            overrides: sylvander_protocol::SessionConfigOverrides::default(),
            text: msg.content.clone(),
            attachments: Vec::new(),
            external_meta,
        },
    )
    .await
    {
        Ok(session_id) => session_id,
        Err(error) => {
            warn!(code = ?error.code, request_id = %error.request_id, "wechat: message denied");
            return "success".into();
        }
    };
    if let Ok(Some(session)) = state.sessions.get(&session_id).await {
        let _ = state
            .ctx
            .bus
            .publish(BusMessage::system_join_session(
                state.agent_id.clone(),
                session_id.clone(),
                session.metadata,
            ))
            .await;
    }

    // Publish to bus
    let _ = state
        .ctx
        .bus
        .publish(BusMessage::user_chat(
            session_id.clone(),
            &msg.from_user_name,
            &msg.content,
        ))
        .await;

    "success".into()
}

async fn find_by_user(
    store: &Arc<dyn SessionStore>,
    instance_id: &str,
    user: &str,
) -> Option<SessionId> {
    let list = store.list_persistent().await.ok()?;
    for s in &list {
        if s.external_meta
            .get("channel_instance_id")
            .and_then(|v| v.as_str())
            == Some(instance_id)
            && s.external_meta
                .get("from_user_name")
                .and_then(|v| v.as_str())
                == Some(user)
        {
            return Some(s.id.clone());
        }
    }
    None
}

// ===========================================================================
// Outgoing: bus → encrypt → reply XML
// ===========================================================================

async fn run_outgoing(ch: Arc<WechatChannel>, ctx: Arc<ChannelContext>) {
    let mut rx = ctx
        .bus
        .subscribe(SubscriptionFilter::all())
        .await
        .expect("subscribe");

    while let Some(msg) = rx.recv().await {
        let MessageKind::Stream(ref ev) = msg.kind else {
            continue;
        };

        // Find from_user_name for this session
        let Some(user_name) = get_user_name(&ctx.sessions, &msg.session_id, &ch.instance_id).await
        else {
            continue;
        };

        let text = match ev {
            StreamEvent::TextDelta { delta } => delta.clone(),
            StreamEvent::Done { text } => {
                send_reply(&ch, &user_name, text);
                continue;
            }
            StreamEvent::ToolCall { tool_name, .. } => format!("🔧 {tool_name}"),
            StreamEvent::ToolResult {
                tool_name,
                output,
                is_error,
                ..
            } => {
                let icon = if *is_error { "❌" } else { "✅" };
                format!("{icon} {tool_name}: {}", truncate_chars(output, 200))
            }
            StreamEvent::ToolApprovalRequired { tools, .. } => {
                let list: Vec<String> = tools.iter().map(|t| t.tool_name.clone()).collect();
                format!("⚠️ approval: {}", list.join(", "))
            }
            StreamEvent::IterationStart { iteration } => {
                format!("💭 thinking (round {iteration})")
            }
            _ => continue,
        };

        send_reply(&ch, &user_name, &text);
    }
}

fn truncate_chars(value: &str, limit: usize) -> &str {
    value
        .char_indices()
        .nth(limit)
        .map_or(value, |(index, _)| &value[..index])
}

async fn get_user_name(
    store: &Arc<dyn SessionStore>,
    sid: &SessionId,
    instance_id: &str,
) -> Option<String> {
    let session = store.get(sid).await.ok()??;
    if session
        .external_meta
        .get("channel_instance_id")
        .and_then(|value| value.as_str())
        != Some(instance_id)
    {
        return None;
    }
    session
        .external_meta
        .get("from_user_name")
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn send_reply(ch: &WechatChannel, to_user: &str, content: &str) {
    // Build text reply XML
    let now = chrono::Utc::now().timestamp();
    let text_xml = format!(
        "<xml><ToUserName><![CDATA[{to_user}]]></ToUserName>\
         <FromUserName><![CDATA[{}]]></FromUserName>\
         <CreateTime>{now}</CreateTime>\
         <MsgType><![CDATA[text]]></MsgType>\
         <Content><![CDATA[{content}]]></Content></xml>",
        ch.crypto.corp_id
    );

    // Encrypt
    let timestamp = now.to_string();
    let nonce = uuid::Uuid::new_v4().to_string();
    match ch.crypto.encrypt(&text_xml, &timestamp, &nonce) {
        Ok(encrypted_xml) => {
            // POST back to WeChat — but enterprise app callbacks are passive,
            // so we need active reply API. For simplicity, this is a no-op
            // (real implementation would call https://qyapi.weixin.qq.com/cgi-bin/message/send).
            warn!(
                "wechat: active reply not implemented (logged only): {}",
                encrypted_xml
            );
        }
        Err(e) => warn!(error = %e, "wechat: encrypt reply failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_output_truncation_is_unicode_safe() {
        assert_eq!(truncate_chars("中文消息", 2), "中文");
    }
}
