//! Agent service boundary.
//!
//! The runtime speaks only in `DomainEvent` and `Action`. Wire messages,
//! transport lifecycle, and protocol adaptation remain behind this service.

use std::path::Path;

use tokio::sync::mpsc;

use crate::client::{ClientEvent, ClientMsg, UnixClient, parse_server_msg};
use crate::event::{Action, DomainEvent};

pub struct AgentService {
    client: UnixClient,
    events: mpsc::UnboundedReceiver<ClientEvent>,
}

impl AgentService {
    pub fn new(socket_path: &Path) -> Self {
        let (client, events) = UnixClient::new(socket_path);
        Self { client, events }
    }

    pub async fn connect(&mut self) -> DomainEvent {
        match self.client.connect().await {
            Ok(()) => DomainEvent::Connected,
            Err(error) => DomainEvent::Disconnected {
                reason: error.to_string(),
            },
        }
    }

    pub async fn recv(&mut self) -> Option<DomainEvent> {
        loop {
            match self.events.recv().await? {
                ClientEvent::Disconnected => {
                    return Some(DomainEvent::Disconnected {
                        reason: "server closed".into(),
                    });
                }
                ClientEvent::Message(message) => {
                    if let Some(event) = parse_server_msg(message) {
                        return Some(event);
                    }
                }
            }
        }
    }

    pub fn try_recv(&mut self) -> Option<DomainEvent> {
        loop {
            match self.events.try_recv().ok()? {
                ClientEvent::Disconnected => {
                    return Some(DomainEvent::Disconnected {
                        reason: "server closed".into(),
                    });
                }
                ClientEvent::Message(message) => {
                    if let Some(event) = parse_server_msg(message) {
                        return Some(event);
                    }
                }
            }
        }
    }

    pub async fn execute(&mut self, action: Action) -> std::io::Result<()> {
        let message = match action {
            Action::SendChat { text, session_id } | Action::SendFeedback { text, session_id } => {
                ClientMsg::Chat { text, session_id }
            }
            Action::SendApprove { call_id, approved } => ClientMsg::Approve { call_id, approved },
            Action::SendAnswer { call_id, answer } => ClientMsg::Answer { call_id, answer },
            Action::Quit => return Ok(()),
        };
        self.client.send(&message).await
    }
}
