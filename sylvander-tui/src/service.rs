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
    events: mpsc::Receiver<ClientEvent>,
}

impl AgentService {
    pub fn new(socket_path: &Path) -> Self {
        let (client, events) = UnixClient::new(socket_path);
        Self { client, events }
    }

    pub async fn connect(&mut self) -> DomainEvent {
        match self.client.connect().await {
            Ok(protocol) => DomainEvent::ProtocolNegotiated {
                version: protocol.version,
                server_name: protocol.server_name,
                capabilities: protocol.capabilities,
            },
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
                    if let Some(event) = parse_server_msg(*message) {
                        return Some(event);
                    }
                }
                ClientEvent::Diagnostic(message) => {
                    return Some(DomainEvent::ProtocolDiagnostic { message });
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
                    if let Some(event) = parse_server_msg(*message) {
                        return Some(event);
                    }
                }
                ClientEvent::Diagnostic(message) => {
                    return Some(DomainEvent::ProtocolDiagnostic { message });
                }
            }
        }
    }

    pub async fn execute(&mut self, action: Action) -> std::io::Result<()> {
        let message = match action {
            Action::HostPreview { .. } => {
                return Err(std::io::Error::other(
                    "host preview must be handled by the local runtime",
                ));
            }
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
            Action::SendApprove {
                session_id,
                call_id,
                approved,
                scope,
                reason,
            } => ClientMsg::Approve {
                session_id,
                call_id,
                approved,
                scope,
                reason,
            },
            Action::SendAnswer {
                session_id,
                call_id,
                answer,
            } => ClientMsg::Answer {
                session_id,
                call_id,
                answer,
            },
            Action::InterruptTurn { session_id } => ClientMsg::Interrupt { session_id },
            Action::ResolvePlan {
                session_id,
                plan_id,
                decision,
            } => ClientMsg::ResolvePlan {
                session_id,
                plan_id,
                decision,
            },
            Action::CancelTask {
                session_id,
                task_id,
            } => ClientMsg::CancelTask {
                session_id,
                task_id,
            },
            Action::RequestSessions => ClientMsg::ListSessions,
            Action::RequestRuntimeInfo => ClientMsg::GetRuntimeInfo,
            Action::DiscoverAgents => ClientMsg::DiscoverAgents,
            Action::CreateSession { request } => ClientMsg::CreateSession { request: *request },
            Action::RequestContext { session_id } => ClientMsg::GetContext { session_id },
            Action::CompactSession { session_id } => ClientMsg::Compact { session_id },
            Action::PreviewWorkspaceRollback { session_id } => {
                ClientMsg::PreviewWorkspaceRollback { session_id }
            }
            Action::ConfirmWorkspaceRollback {
                session_id,
                expected_turn_id,
            } => ClientMsg::RollbackWorkspace {
                session_id,
                expected_turn_id,
            },
            Action::InspectCodingSession { session_id } => {
                ClientMsg::InspectCodingSession { session_id }
            }
            Action::AcceptCodingSession { session_id } => {
                ClientMsg::AcceptCodingSession { session_id }
            }
            Action::DiscardCodingSession { session_id } => {
                ClientMsg::DiscardCodingSession { session_id }
            }
            Action::SelectModel {
                session_id,
                model,
                reasoning_effort,
            } => ClientMsg::SelectModel {
                session_id: Some(session_id),
                model,
                reasoning_effort,
            },
            Action::SelectPermissions {
                session_id,
                profile,
            } => ClientMsg::SelectPermissions {
                session_id: Some(session_id),
                profile,
            },
            Action::LoadSession { session_id } => ClientMsg::LoadSession { session_id },
            Action::ReconcileSession { session_id } => ClientMsg::ReattachSession { session_id },
            Action::RenameSession { session_id, label } => {
                ClientMsg::RenameSession { session_id, label }
            }
            Action::ArchiveSession { session_id } => ClientMsg::ArchiveSession { session_id },
            Action::RestoreSession { session_id } => ClientMsg::RestoreSession { session_id },
            Action::DeleteSession { session_id } => ClientMsg::DeleteSession { session_id },
            Action::UserProfile { request } => ClientMsg::UserProfile { request },
            Action::SubmitFeedback { feedback } => ClientMsg::SubmitFeedback { feedback },
            Action::RequestMemoryConfirmations { session_id } => ClientMsg::MemoryConfirmation {
                request: sylvander_protocol::MemoryConfirmationRequest::List {
                    version: sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                    session_id,
                },
            },
            Action::ResolveMemoryConfirmation {
                session_id,
                candidate_id,
                expected_revision,
                decision,
            } => ClientMsg::MemoryConfirmation {
                request: sylvander_protocol::MemoryConfirmationRequest::Decide {
                    version: sylvander_protocol::MEMORY_CONFIRMATION_PROTOCOL_VERSION,
                    session_id,
                    candidate_id,
                    expected_revision,
                    decision,
                },
            },
            Action::CopyText { .. }
            | Action::EditDraft
            | Action::InspectWorkspaceDiff { .. }
            | Action::ReviewWorkspaceChanges { .. }
            | Action::InspectConfig
            | Action::RunDoctor { .. } => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "local actions belong to the terminal runtime",
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
