//! Runtime-owned lifecycle for isolated, read-only background Agent work.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::{Mutex, oneshot};

use sylvander_agent::execution_ports::AgentExecutionPorts;
use sylvander_agent::kernel::agent_loop::{self, AgentLoop};
use sylvander_agent::task_gate::TaskGate;
use sylvander_agent::turn::conversation::ConversationSnapshot;
use sylvander_agent::turn::request::AgentTurnRequest;
use sylvander_api::{AgentId, BusMessage, SessionId, StreamEvent};
use sylvander_channel::MessageBus;
use sylvander_llm_core::ChatMessage;

use super::interaction::publish_interaction_timeout;

const BACKGROUND_TASK_TIMEOUT_SECS: u64 = 10 * 60;

pub(super) struct ActiveBackgroundTask {
    pub(super) session_id: SessionId,
    pub(super) cancel: oneshot::Sender<()>,
}

pub(super) struct BusTaskGate {
    pub(super) bus: Arc<dyn MessageBus>,
    pub(super) agent_id: AgentId,
    pub(super) session_id: SessionId,
    pub(super) kernel: AgentLoop,
    pub(super) request: AgentTurnRequest,
    pub(super) ports: AgentExecutionPorts,
    pub(super) tasks: Arc<Mutex<HashMap<String, ActiveBackgroundTask>>>,
}

#[async_trait::async_trait]
impl TaskGate for BusTaskGate {
    async fn start(&self, purpose: String, prompt: String) -> Result<String, String> {
        if prompt.trim().is_empty() {
            return Err("background task prompt cannot be empty".into());
        }
        let task_id = uuid::Uuid::new_v4().to_string();
        let (cancel, mut cancelled) = oneshot::channel();
        self.tasks.lock().await.insert(
            task_id.clone(),
            ActiveBackgroundTask {
                session_id: self.session_id.clone(),
                cancel,
            },
        );
        let _ = self
            .bus
            .publish(BusMessage::stream_event(
                self.session_id.clone(),
                self.agent_id.clone(),
                StreamEvent::TaskStarted {
                    task_id: task_id.clone(),
                    owner: self.agent_id.0.clone(),
                    purpose,
                },
            ))
            .await;

        let bus = self.bus.clone();
        let agent_id = self.agent_id.clone();
        let session_id = self.session_id.clone();
        let kernel = self.kernel.clone();
        let mut request = self.request.clone();
        let ports = self.ports.clone();
        let tasks = self.tasks.clone();
        let running_id = task_id.clone();
        tokio::spawn(async move {
            request.conversation = ConversationSnapshot::new(vec![ChatMessage::user(prompt)]);
            let mut stream = Box::pin(agent_loop::run_stream(&kernel, request, ports));
            let deadline = tokio::time::sleep(Duration::from_secs(BACKGROUND_TASK_TIMEOUT_SECS));
            tokio::pin!(deadline);
            loop {
                let event = tokio::select! {
                    biased;
                    _ = &mut cancelled => {
                        let _ = bus.publish(BusMessage::stream_event(
                            session_id.clone(),
                            agent_id.clone(),
                            StreamEvent::TaskCancelled {
                                task_id: running_id.clone(),
                                reason: "cancelled by user".into(),
                            },
                        )).await;
                        break;
                    }
                    () = &mut deadline => {
                        publish_interaction_timeout(
                            &bus,
                            &session_id,
                            &agent_id,
                            sylvander_api::InteractionTimeoutKind::Task,
                            &running_id,
                            BACKGROUND_TASK_TIMEOUT_SECS,
                            sylvander_api::TimeoutRecovery::NarrowScope,
                        ).await;
                        let _ = bus.publish(BusMessage::stream_event(
                            session_id.clone(),
                            agent_id.clone(),
                            StreamEvent::TaskFailed {
                                task_id: running_id.clone(),
                                error: format!(
                                    "background task timed out after {BACKGROUND_TASK_TIMEOUT_SECS}s"
                                ),
                            },
                        )).await;
                        break;
                    }
                    event = stream.next() => event,
                };
                let Some(event) = event else { break };
                let public = match event {
                    sylvander_agent::turn::event::AgentEvent::IterationStart { iteration } => {
                        Some(StreamEvent::TaskProgress {
                            task_id: running_id.clone(),
                            message: format!("iteration {iteration}"),
                        })
                    }
                    sylvander_agent::turn::event::AgentEvent::ToolCallStart { name, .. } => {
                        Some(StreamEvent::TaskProgress {
                            task_id: running_id.clone(),
                            message: format!("running {name}"),
                        })
                    }
                    sylvander_agent::turn::event::AgentEvent::Done(outcome) => {
                        Some(StreamEvent::TaskCompleted {
                            task_id: running_id.clone(),
                            summary: outcome.final_response.text(),
                        })
                    }
                    sylvander_agent::turn::event::AgentEvent::Error(error) => {
                        Some(StreamEvent::TaskFailed {
                            task_id: running_id.clone(),
                            error: error.to_string(),
                        })
                    }
                    _ => None,
                };
                let terminal = matches!(
                    public,
                    Some(StreamEvent::TaskCompleted { .. } | StreamEvent::TaskFailed { .. })
                );
                if let Some(event) = public {
                    let _ = bus
                        .publish(BusMessage::stream_event(
                            session_id.clone(),
                            agent_id.clone(),
                            event,
                        ))
                        .await;
                }
                if terminal {
                    break;
                }
            }
            tasks.lock().await.remove(&running_id);
        });
        Ok(task_id)
    }
}
