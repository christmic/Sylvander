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
            Action::SendChat {
                text,
                attachments,
                session_id,
                workspace,
            } => ClientMsg::Chat {
                text,
                attachments,
                session_id,
                workspace: Some(workspace),
            },
            Action::SendFeedback { text, session_id } => ClientMsg::Chat {
                text,
                attachments: Vec::new(),
                session_id,
                workspace: None,
            },
            Action::SendApprove {
                call_id,
                approved,
                scope,
            } => ClientMsg::Approve {
                call_id,
                approved,
                scope,
            },
            Action::SendAnswer { call_id, answer } => ClientMsg::Answer { call_id, answer },
            Action::InterruptTurn { session_id } => ClientMsg::Interrupt { session_id },
            Action::ResolvePlan { plan_id, decision } => {
                ClientMsg::ResolvePlan { plan_id, decision }
            }
            Action::CancelTask {
                session_id,
                task_id,
            } => ClientMsg::CancelTask {
                session_id,
                task_id,
            },
            Action::RequestSessions => ClientMsg::ListSessions,
            Action::RequestRuntimeInfo => ClientMsg::GetRuntimeInfo,
            Action::RequestContext { session_id } => ClientMsg::GetContext { session_id },
            Action::CompactSession { session_id } => ClientMsg::Compact { session_id },
            Action::SelectModel {
                model,
                reasoning_effort,
            } => ClientMsg::SelectModel {
                model,
                reasoning_effort,
            },
            Action::SelectPermissions { profile } => ClientMsg::SelectPermissions { profile },
            Action::LoadSession { session_id } => ClientMsg::LoadSession { session_id },
            Action::RenameSession { session_id, label } => {
                ClientMsg::RenameSession { session_id, label }
            }
            Action::ArchiveSession { session_id } => ClientMsg::ArchiveSession { session_id },
            Action::RestoreSession { session_id } => ClientMsg::RestoreSession { session_id },
            Action::DeleteSession { session_id } => ClientMsg::DeleteSession { session_id },
            Action::CopyText { .. } | Action::EditDraft => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "clipboard actions belong to the terminal runtime",
                ));
            }
            Action::ForkSession {
                session_id,
                completed_turns,
                checkpoint,
            } => ClientMsg::ForkSession {
                session_id,
                completed_turns,
                checkpoint,
            },
            Action::Quit => return Ok(()),
        };
        self.client.send(&message).await
    }
}
