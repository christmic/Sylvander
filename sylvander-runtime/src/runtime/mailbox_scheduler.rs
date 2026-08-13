//! Bounded automatic execution and crash recovery for durable Agent mailboxes.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use sylvander_api::{
    AgentInstanceId, BusMessage, CoordinationMessageId, MessageId, MessageKind, Recipient, Sender,
    SessionId,
};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tracing::warn;

use super::{RuntimeError, RuntimeRevisionProvider};
use crate::coordination::governance::GovernancePolicy;
use crate::coordination::mailbox::{
    AgentMessageTurn, BACKGROUND_TASK_TTL_SECONDS, CoordinationMessage, CoordinationMessageKind,
};
use crate::coordination::service::{
    CancelTaskRequest, CoordinationService, DEFAULT_ARBITRATION_TTL_SECONDS,
    DispatchMessageOutcome, DispatchMessageRequest,
};
use crate::observability::{RuntimeCoordinationOutcome, RuntimeEvent, RuntimeObservability};
use crate::storage::agent_instance::AgentInstanceStore;
use crate::storage::coordination::CoordinationStore;
use crate::storage::session::{SessionStore, SqliteSessionStore, TurnState};

const MAILBOX_LEASE_SECONDS: u64 = 30;
const MAX_CONCURRENT_RECIPIENTS: usize = 16;
const DURABLE_MAILBOX_SCAN_SECONDS: u64 = 1;

pub(super) struct AgentMailboxScheduler {
    wake: mpsc::UnboundedSender<AgentInstanceId>,
    task: JoinHandle<()>,
}

impl AgentMailboxScheduler {
    pub(super) fn start(
        store: Arc<SqliteSessionStore>,
        revisions: Arc<RuntimeRevisionProvider>,
        observability: RuntimeObservability,
    ) -> Self {
        let (wake, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_scheduler(receiver, store, revisions, observability));
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
    observability: RuntimeObservability,
) {
    let mut queued = VecDeque::new();
    let mut known = HashSet::new();
    let mut rerun = HashSet::new();
    let mut active = JoinSet::new();
    let mut receiver_open = true;
    let mut durable_scan = tokio::time::interval(Duration::from_secs(DURABLE_MAILBOX_SCAN_SECONDS));
    durable_scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        while active.len() < MAX_CONCURRENT_RECIPIENTS
            && let Some(recipient) = queued.pop_front()
        {
            let store = store.clone();
            let revisions = revisions.clone();
            let observability = observability.clone();
            active.spawn(async move {
                let wake = match Box::pin(drain_recipient(
                    &store,
                    &revisions,
                    &observability,
                    &recipient,
                ))
                .await
                {
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
            _ = durable_scan.tick() => {
                let recovered = recover_background_dispatches(&store, &observability).await;
                if let Err(error) = recovered {
                    warn!(%error, "durable background outbox recovery failed");
                }
                match store.recoverable_message_recipients(crate::session::now_secs()).await {
                    Ok(recipients) => {
                        for recipient in recipients {
                            if known.insert(recipient.clone()) {
                                queued.push_back(recipient);
                            } else {
                                rerun.insert(recipient);
                            }
                        }
                    }
                    Err(error) => warn!(%error, "durable Agent mailbox scan failed"),
                }
            }
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

async fn recover_background_dispatches(
    store: &Arc<SqliteSessionStore>,
    observability: &RuntimeObservability,
) -> Result<(), RuntimeError> {
    let now = crate::session::now_secs();
    let service = CoordinationService::new(
        store.clone(),
        GovernancePolicy::default(),
        DEFAULT_ARBITRATION_TTL_SECONDS,
    );
    for task in store
        .undispatched_background_tasks()
        .await
        .map_err(|error| RuntimeError::Store(error.to_string()))?
    {
        let Some(recipient) = task.assigned_to.clone() else {
            continue;
        };
        let expires_at = task
            .created_at
            .checked_add(BACKGROUND_TASK_TTL_SECONDS)
            .ok_or_else(|| RuntimeError::Coordination("background deadline overflow".into()))?;
        if expires_at <= now {
            service
                .cancel_task(
                    CancelTaskRequest {
                        task_id: task.task_id,
                        session_id: task.session_id,
                        actor: task.created_by,
                    },
                    now,
                )
                .await
                .map_err(|error| RuntimeError::Coordination(error.to_string()))?;
            continue;
        }
        let digest = task
            .task_id
            .0
            .strip_prefix("background-task:")
            .ok_or_else(|| RuntimeError::Coordination("invalid background task id".into()))?;
        let outcome = service
            .dispatch_message(
                DispatchMessageRequest {
                    message_id: CoordinationMessageId::new(format!("background-message:{digest}")),
                    session_id: task.session_id.clone(),
                    sender_instance_id: task.created_by,
                    recipient_instance_id: recipient,
                    task_id: Some(task.task_id),
                    kind: CoordinationMessageKind::Task,
                    payload: task.objective,
                    max_hops: 1,
                    expires_at,
                },
                now,
            )
            .await
            .map_err(|error| RuntimeError::Coordination(error.to_string()))?;
        let outcome = match outcome {
            DispatchMessageOutcome::Enqueued(_) => RuntimeCoordinationOutcome::Enqueued,
            DispatchMessageOutcome::EnqueuedByModerator { .. } => {
                RuntimeCoordinationOutcome::ModeratorAuthorized
            }
            DispatchMessageOutcome::RequiresArbitration { .. } => {
                RuntimeCoordinationOutcome::ArbitrationRequired
            }
            DispatchMessageOutcome::RejectedByModerator { .. } => {
                RuntimeCoordinationOutcome::ModeratorRejected
            }
        };
        observability.record(RuntimeEvent::CoordinationTransition {
            session_id: task.session_id,
            outcome,
        });
    }
    Ok(())
}

async fn drain_recipient(
    store: &Arc<SqliteSessionStore>,
    revisions: &Arc<RuntimeRevisionProvider>,
    observability: &RuntimeObservability,
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
                if let Some(moderator) = Box::pin(execute_or_escalate(
                    store,
                    revisions,
                    observability,
                    &service,
                    &message,
                    &receipt,
                ))
                .await?
                {
                    return Ok(Some(moderator));
                }
            }
            Some(_) => {
                let case = service
                    .escalate_mailbox_turn(&message, &receipt, crate::session::now_secs())
                    .await
                    .map_err(|error| RuntimeError::Coordination(error.to_string()))?;
                record_mailbox_escalation(observability, &case.session_id);
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
        if let Some(moderator) = Box::pin(execute_or_escalate(
            store,
            revisions,
            observability,
            &service,
            &message,
            &receipt,
        ))
        .await?
        {
            return Ok(Some(moderator));
        }
    }
    Ok(None)
}

async fn execute_or_escalate(
    store: &Arc<SqliteSessionStore>,
    revisions: &Arc<RuntimeRevisionProvider>,
    observability: &RuntimeObservability,
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
            record_mailbox_escalation(observability, &case.session_id);
            Ok(Some(case.moderator_instance_id))
        }
        None => Err(execution_error),
    }
}

fn record_mailbox_escalation(observability: &RuntimeObservability, session_id: &SessionId) {
    observability.record(RuntimeEvent::CoordinationTransition {
        session_id: session_id.clone(),
        outcome: RuntimeCoordinationOutcome::MailboxEscalated,
    });
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
