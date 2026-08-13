//! Bounded automatic execution and crash recovery for durable Agent mailboxes.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use sylvander_api::{AgentInstanceId, BusMessage, MessageId, MessageKind, Recipient, Sender};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tracing::warn;

use super::{RuntimeError, RuntimeRevisionProvider};
use crate::coordination::governance::GovernancePolicy;
use crate::coordination::mailbox::{AgentMessageTurn, CoordinationMessage};
use crate::coordination::service::{CoordinationService, DEFAULT_ARBITRATION_TTL_SECONDS};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::{SessionStore, SqliteSessionStore, TurnState};

const MAILBOX_LEASE_SECONDS: u64 = 30;
const MAX_CONCURRENT_RECIPIENTS: usize = 16;

pub(super) struct AgentMailboxScheduler {
    wake: mpsc::UnboundedSender<AgentInstanceId>,
    task: JoinHandle<()>,
}

impl AgentMailboxScheduler {
    pub(super) fn start(
        store: Arc<SqliteSessionStore>,
        revisions: Arc<RuntimeRevisionProvider>,
    ) -> Self {
        let (wake, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_scheduler(receiver, store, revisions));
        Self { wake, task }
    }

    pub(super) fn wake(&self, recipient: AgentInstanceId) {
        let _ = self.wake.send(recipient);
    }
}

impl Drop for AgentMailboxScheduler {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run_scheduler(
    mut receiver: mpsc::UnboundedReceiver<AgentInstanceId>,
    store: Arc<SqliteSessionStore>,
    revisions: Arc<RuntimeRevisionProvider>,
) {
    let mut queued = VecDeque::new();
    let mut known = HashSet::new();
    let mut rerun = HashSet::new();
    let mut active = JoinSet::new();
    let mut receiver_open = true;
    loop {
        while active.len() < MAX_CONCURRENT_RECIPIENTS
            && let Some(recipient) = queued.pop_front()
        {
            let store = store.clone();
            let revisions = revisions.clone();
            active.spawn(async move {
                let wake = match Box::pin(drain_recipient(&store, &revisions, &recipient)).await {
                    Ok(wake) => wake,
                    Err(error) => {
                        warn!(%recipient, %error, "automatic Agent mailbox delivery paused");
                        None
                    }
                };
                (recipient, wake)
            });
        }
        tokio::select! {
            wake = receiver.recv(), if receiver_open => match wake {
                Some(recipient) if known.insert(recipient.clone()) => queued.push_back(recipient),
                Some(recipient) => {
                    rerun.insert(recipient);
                }
                None => receiver_open = false,
            },
            completed = active.join_next(), if !active.is_empty() => {
                if let Some(Ok((recipient, wake))) = completed {
                    if rerun.remove(&recipient) {
                        queued.push_back(recipient);
                    } else {
                        known.remove(&recipient);
                    }
                    if let Some(wake) = wake {
                        if known.insert(wake.clone()) {
                            queued.push_back(wake);
                        } else {
                            rerun.insert(wake);
                        }
                    }
                }
            }
        }
        if !receiver_open && active.is_empty() && queued.is_empty() {
            break;
        }
    }
}

async fn drain_recipient(
    store: &Arc<SqliteSessionStore>,
    revisions: &Arc<RuntimeRevisionProvider>,
    recipient: &AgentInstanceId,
) -> Result<Option<AgentInstanceId>, RuntimeError> {
    let service = CoordinationService::new(
        store.clone(),
        GovernancePolicy::default(),
        DEFAULT_ARBITRATION_TTL_SECONDS,
    );
    for (message, receipt) in store
        .recoverable_message_turns(recipient)
        .await
        .map_err(|error| RuntimeError::Store(error.to_string()))?
    {
        match store
            .turn(&receipt.session_id, &receipt.turn_id)
            .await
            .map_err(|error| RuntimeError::Store(error.to_string()))?
        {
            Some(turn) if turn.state == TurnState::Completed => {
                service
                    .acknowledge_message(&message, recipient, crate::session::now_secs())
                    .await
                    .map_err(|error| RuntimeError::Coordination(error.to_string()))?;
            }
            None => {
                if let Some(moderator) =
                    execute_or_escalate(store, revisions, &service, &message, &receipt).await?
                {
                    return Ok(Some(moderator));
                }
            }
            Some(_) => {
                let case = service
                    .escalate_mailbox_turn(&message, &receipt, crate::session::now_secs())
                    .await
                    .map_err(|error| RuntimeError::Coordination(error.to_string()))?;
                return Ok(Some(case.moderator_instance_id));
            }
        }
    }
    while let Some(claim) = service
        .claim_next_message(recipient, crate::session::now_secs(), MAILBOX_LEASE_SECONDS)
        .await
        .map_err(|error| RuntimeError::Coordination(error.to_string()))?
    {
        let turn_id = format!(
            "agent-message:{:x}",
            Sha256::digest(claim.message.message_id.0.as_bytes())
        );
        let (message, receipt) = service
            .prepare_message_turn(&claim, &turn_id, crate::session::now_secs())
            .await
            .map_err(|error| RuntimeError::Coordination(error.to_string()))?;
        if let Some(moderator) =
            execute_or_escalate(store, revisions, &service, &message, &receipt).await?
        {
            return Ok(Some(moderator));
        }
    }
    Ok(None)
}

async fn execute_or_escalate(
    store: &Arc<SqliteSessionStore>,
    revisions: &Arc<RuntimeRevisionProvider>,
    service: &CoordinationService<SqliteSessionStore>,
    message: &CoordinationMessage,
    receipt: &AgentMessageTurn,
) -> Result<Option<AgentInstanceId>, RuntimeError> {
    let Err(execution_error) =
        execute_message_turn(store, revisions, service, message, receipt).await
    else {
        return Ok(None);
    };
    let turn = store
        .turn(&receipt.session_id, &receipt.turn_id)
        .await
        .map_err(|error| RuntimeError::Store(error.to_string()))?;
    match turn {
        Some(turn) if turn.state == TurnState::Completed => {
            service
                .acknowledge_message(
                    message,
                    &message.recipient_instance_id,
                    crate::session::now_secs(),
                )
                .await
                .map_err(|error| RuntimeError::Coordination(error.to_string()))?;
            Ok(None)
        }
        Some(_) => {
            warn!(
                message_id = %message.message_id.0,
                turn_id = %receipt.turn_id,
                error = %execution_error,
                "Agent mailbox turn failed after becoming durable; escalating"
            );
            let case = service
                .escalate_mailbox_turn(message, receipt, crate::session::now_secs())
                .await
                .map_err(|error| RuntimeError::Coordination(error.to_string()))?;
            Ok(Some(case.moderator_instance_id))
        }
        None => Err(execution_error),
    }
}

async fn execute_message_turn(
    store: &Arc<SqliteSessionStore>,
    revisions: &Arc<RuntimeRevisionProvider>,
    service: &CoordinationService<SqliteSessionStore>,
    message: &CoordinationMessage,
    receipt: &AgentMessageTurn,
) -> Result<(), RuntimeError> {
    let membership = store
        .session_membership(&message.session_id)
        .await
        .map_err(|error| RuntimeError::Store(error.to_string()))?
        .ok_or_else(|| RuntimeError::Coordination("mailbox membership disappeared".into()))?;
    let participant = |id: &AgentInstanceId| {
        membership
            .participants
            .iter()
            .find(|participant| &participant.instance_id == id)
            .ok_or_else(|| RuntimeError::Coordination("mailbox participant disappeared".into()))
    };
    let recipient = participant(&message.recipient_instance_id)?;
    let sender = participant(&message.sender_instance_id)?;
    let configured = revisions
        .configured_revision(
            &recipient.definition.agent_id,
            recipient.definition.revision,
        )
        .await?;
    let prompt = format!(
        "[inter-agent {:?}; message_id={}; sender={}; task={}]\n{}",
        message.kind,
        message.message_id.0,
        message.sender_instance_id.0,
        message
            .task_id
            .as_ref()
            .map_or("none", |task| task.0.as_str()),
        message.payload
    );
    configured
        .run
        .execute_durable_message(
            BusMessage {
                session_id: message.session_id.clone(),
                sender: Sender::AgentInstance {
                    instance_id: sender.instance_id.clone(),
                    agent_id: sender.definition.agent_id.clone(),
                },
                recipient: Recipient::AgentInstance {
                    instance_id: recipient.instance_id.clone(),
                    agent_id: recipient.definition.agent_id.clone(),
                },
                kind: MessageKind::Chat,
                payload: prompt,
                attachments: Vec::new(),
                timestamp: crate::session::now_secs(),
                id: MessageId::new(),
            },
            receipt.turn_id.clone(),
        )
        .await
        .map_err(|error| RuntimeError::Engine(error.to_string()))?;
    service
        .acknowledge_message(
            message,
            &message.recipient_instance_id,
            crate::session::now_secs(),
        )
        .await
        .map_err(|error| RuntimeError::Coordination(error.to_string()))?;
    Ok(())
}
