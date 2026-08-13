//! SQLite persistence for governed multi-Agent coordination facts.

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use sylvander_api::{AgentInstanceId, SessionId, TaskId};

use crate::coordination::task::{CoordinationTask, CoordinationTaskState};
use crate::coordination::topology::{AgentRelation, AgentRelationKind, SessionTopology};
use crate::session::membership::SessionMembership;
use crate::storage::session::{SessionStoreError, SqliteSessionStore};

#[async_trait]
pub trait CoordinationStore: Send + Sync {
    async fn save_topology(
        &self,
        topology: &SessionTopology,
        membership: &SessionMembership,
        expected_revision: Option<u64>,
    ) -> Result<(), SessionStoreError>;

    async fn topology(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionTopology>, SessionStoreError>;

    async fn create_task(&self, task: &CoordinationTask) -> Result<(), SessionStoreError>;

    async fn update_task(
        &self,
        task: &CoordinationTask,
        expected_revision: u64,
    ) -> Result<(), SessionStoreError>;

    async fn task(&self, task_id: &TaskId) -> Result<Option<CoordinationTask>, SessionStoreError>;
}

#[async_trait]
impl CoordinationStore for SqliteSessionStore {
    async fn save_topology(
        &self,
        topology: &SessionTopology,
        membership: &SessionMembership,
        expected_revision: Option<u64>,
    ) -> Result<(), SessionStoreError> {
        topology
            .validate(membership)
            .map_err(|error| SessionStoreError::Invalid(error.to_string()))?;
        let topology = topology.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let membership_revision = transaction
                .query_row(
                    "SELECT membership_revision FROM session_governance WHERE session_id=?1",
                    [&topology.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let Some(membership_revision) = membership_revision else {
                return Err(SessionStoreError::Invalid(
                    "topology requires durable Session membership".into(),
                ));
            };
            let actual_membership_revision =
                checked_u64(membership_revision, "membership revision")?;
            if actual_membership_revision != topology.membership_revision {
                return Err(SessionStoreError::MembershipConflict {
                    expected: Some(topology.membership_revision),
                    actual: Some(actual_membership_revision),
                });
            }
            let actual_revision = transaction
                .query_row(
                    "SELECT topology_revision FROM session_topology WHERE session_id=?1",
                    [&topology.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map(|value| checked_u64(value, "topology revision"))
                .transpose()?;
            if actual_revision != expected_revision {
                return Err(SessionStoreError::TopologyConflict {
                    expected: expected_revision,
                    actual: actual_revision,
                });
            }
            let next_revision = match expected_revision {
                None => 0,
                Some(revision) => revision.checked_add(1).ok_or_else(|| {
                    SessionStoreError::Invalid("topology revision overflow".into())
                })?,
            };
            if topology.topology_revision != next_revision {
                return Err(SessionStoreError::Invalid(
                    "next topology revision is not sequential".into(),
                ));
            }

            transaction.execute(
                "DELETE FROM session_topology WHERE session_id=?1",
                [&topology.session_id.0],
            )?;
            transaction.execute(
                "INSERT INTO session_topology \
                 (session_id,membership_revision,topology_revision,updated_at) \
                 VALUES (?1,?2,?3,?4)",
                params![
                    topology.session_id.0,
                    checked_i64(topology.membership_revision, "membership revision")?,
                    checked_i64(topology.topology_revision, "topology revision")?,
                    topology.updated_at,
                ],
            )?;
            for (ordinal, relation) in topology.relations.iter().enumerate() {
                transaction.execute(
                    "INSERT INTO agent_relations \
                     (session_id,relation_ordinal,source_instance_id,target_instance_id,relation_kind,created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        topology.session_id.0,
                        i64::try_from(ordinal).map_err(|_| SessionStoreError::Invalid(
                            "Agent relation ordinal exceeds SQLite range".into()
                        ))?,
                        relation.source.0,
                        relation.target.0,
                        encode_relation_kind(relation.kind),
                        relation.created_at,
                    ],
                )?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn topology(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<SessionTopology>, SessionStoreError> {
        let session_id = session_id.clone();
        self.run(move |connection| {
            let header = connection
                .query_row(
                    "SELECT membership_revision,topology_revision,updated_at \
                     FROM session_topology WHERE session_id=?1",
                    [&session_id.0],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((membership_revision, topology_revision, updated_at)) = header else {
                return Ok(None);
            };
            let mut statement = connection.prepare(
                "SELECT source_instance_id,target_instance_id,relation_kind,created_at \
                 FROM agent_relations WHERE session_id=?1 ORDER BY relation_ordinal",
            )?;
            let rows = statement.query_map([&session_id.0], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            let relations = rows
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(source, target, kind, created_at)| {
                    Ok(AgentRelation {
                        source: AgentInstanceId::new(source),
                        target: AgentInstanceId::new(target),
                        kind: decode_relation_kind(&kind)?,
                        created_at,
                    })
                })
                .collect::<Result<Vec<_>, SessionStoreError>>()?;
            Ok(Some(SessionTopology {
                session_id,
                membership_revision: checked_u64(membership_revision, "membership revision")?,
                topology_revision: checked_u64(topology_revision, "topology revision")?,
                relations,
                updated_at,
            }))
        })
        .await
    }

    async fn create_task(&self, task: &CoordinationTask) -> Result<(), SessionStoreError> {
        if task.revision != 0
            || task.objective.trim().is_empty()
            || task.token_budget == 0
            || task.consumed_tokens > task.token_budget
            || task.handoff_count > task.max_handoffs
        {
            return Err(SessionStoreError::Invalid(
                "invalid new coordination task".into(),
            ));
        }
        let task = task.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let actual_revision = transaction
                .query_row(
                    "SELECT revision FROM coordination_tasks WHERE task_id=?1",
                    [&task.task_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .map(|value| checked_u64(value, "task revision"))
                .transpose()?;
            if actual_revision.is_some() {
                return Err(SessionStoreError::TaskConflict {
                    task_id: task.task_id,
                    expected: None,
                    actual: actual_revision,
                });
            }
            let membership_revision = transaction
                .query_row(
                    "SELECT membership_revision FROM session_governance WHERE session_id=?1",
                    [&task.session_id.0],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if membership_revision
                .map(|value| checked_u64(value, "membership revision"))
                .transpose()?
                != Some(task.membership_revision)
            {
                return Err(SessionStoreError::Invalid(
                    "task membership revision is not current".into(),
                ));
            }
            for actor in [
                &task.created_by,
                task.assigned_to.as_ref().unwrap_or(&task.created_by),
            ] {
                let exists = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM session_agent_instances \
                     WHERE session_id=?1 AND instance_id=?2)",
                    params![task.session_id.0, actor.0],
                    |row| row.get::<_, bool>(0),
                )?;
                if !exists {
                    return Err(SessionStoreError::Invalid(
                        "task references an unknown Agent instance".into(),
                    ));
                }
            }
            transaction.execute(
                "INSERT INTO coordination_tasks \
                 (task_id,session_id,membership_revision,parent_task_id,created_by_instance_id,
                  assigned_to_instance_id,objective,state,token_budget,consumed_tokens,
                  max_handoffs,handoff_count,revision,created_at,updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0,?13,?14)",
                params![
                    task.task_id.0,
                    task.session_id.0,
                    checked_i64(task.membership_revision, "membership revision")?,
                    task.parent_task_id.as_ref().map(|id| &id.0),
                    task.created_by.0,
                    task.assigned_to.as_ref().map(|id| &id.0),
                    task.objective,
                    encode_task_state(task.state),
                    checked_i64(task.token_budget, "task token budget")?,
                    checked_i64(task.consumed_tokens, "task consumed tokens")?,
                    i64::from(task.max_handoffs),
                    i64::from(task.handoff_count),
                    task.created_at,
                    task.updated_at,
                ],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn update_task(
        &self,
        task: &CoordinationTask,
        expected_revision: u64,
    ) -> Result<(), SessionStoreError> {
        let task = task.clone();
        self.run(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let Some(current) = load_task(&transaction, &task.task_id)? else {
                return Err(SessionStoreError::TaskConflict {
                    task_id: task.task_id,
                    expected: Some(expected_revision),
                    actual: None,
                });
            };
            if current.revision != expected_revision {
                return Err(SessionStoreError::TaskConflict {
                    task_id: task.task_id,
                    expected: Some(expected_revision),
                    actual: Some(current.revision),
                });
            }
            let next_revision = expected_revision.checked_add(1).ok_or_else(|| {
                SessionStoreError::Invalid("task revision overflow".into())
            })?;
            if task.revision != next_revision
                || task.session_id != current.session_id
                || task.membership_revision != current.membership_revision
                || task.parent_task_id != current.parent_task_id
                || task.created_by != current.created_by
                || task.assigned_to != current.assigned_to
                || task.objective != current.objective
                || task.token_budget != current.token_budget
                || task.max_handoffs != current.max_handoffs
                || task.handoff_count != current.handoff_count
                || task.created_at != current.created_at
                || task.updated_at < current.updated_at
                || task.consumed_tokens < current.consumed_tokens
                || task.consumed_tokens > task.token_budget
                || !current.state.can_transition_to(task.state)
            {
                return Err(SessionStoreError::Invalid(
                    "task update violates immutable facts or lifecycle rules".into(),
                ));
            }
            let durable_membership_revision = transaction.query_row(
                "SELECT membership_revision FROM session_governance WHERE session_id=?1",
                [&task.session_id.0],
                |row| row.get::<_, i64>(0),
            )?;
            if checked_u64(durable_membership_revision, "membership revision")?
                != task.membership_revision
            {
                return Err(SessionStoreError::Invalid(
                    "task update requires membership reconciliation".into(),
                ));
            }
            let changed = transaction.execute(
                "UPDATE coordination_tasks SET state=?1,consumed_tokens=?2,revision=?3,updated_at=?4 \
                 WHERE task_id=?5 AND revision=?6",
                params![
                    encode_task_state(task.state),
                    checked_i64(task.consumed_tokens, "task consumed tokens")?,
                    checked_i64(task.revision, "task revision")?,
                    task.updated_at,
                    task.task_id.0,
                    checked_i64(expected_revision, "expected task revision")?,
                ],
            )?;
            if changed != 1 {
                return Err(SessionStoreError::TaskConflict {
                    task_id: task.task_id,
                    expected: Some(expected_revision),
                    actual: None,
                });
            }
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    async fn task(&self, task_id: &TaskId) -> Result<Option<CoordinationTask>, SessionStoreError> {
        let task_id = task_id.clone();
        self.run(move |connection| load_task(connection, &task_id))
            .await
    }
}

fn load_task(
    connection: &rusqlite::Connection,
    task_id: &TaskId,
) -> Result<Option<CoordinationTask>, SessionStoreError> {
    connection
        .query_row(
            "SELECT session_id,membership_revision,parent_task_id,created_by_instance_id,
                    assigned_to_instance_id,objective,state,token_budget,consumed_tokens,
                    max_handoffs,handoff_count,revision,created_at,updated_at
             FROM coordination_tasks WHERE task_id=?1",
            [&task_id.0],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                ))
            },
        )
        .optional()?
        .map(|row| decode_task(task_id.clone(), row))
        .transpose()
}

type EncodedTask = (
    String,
    i64,
    Option<String>,
    String,
    Option<String>,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
);

fn decode_task(task_id: TaskId, row: EncodedTask) -> Result<CoordinationTask, SessionStoreError> {
    Ok(CoordinationTask {
        task_id,
        session_id: SessionId::new(row.0),
        membership_revision: checked_u64(row.1, "membership revision")?,
        parent_task_id: row.2.map(TaskId::new),
        created_by: AgentInstanceId::new(row.3),
        assigned_to: row.4.map(AgentInstanceId::new),
        objective: row.5,
        state: decode_task_state(&row.6)?,
        token_budget: checked_u64(row.7, "task token budget")?,
        consumed_tokens: checked_u64(row.8, "task consumed tokens")?,
        max_handoffs: checked_u32(row.9, "task maximum handoffs")?,
        handoff_count: checked_u32(row.10, "task handoff count")?,
        revision: checked_u64(row.11, "task revision")?,
        created_at: row.12,
        updated_at: row.13,
    })
}

const fn encode_task_state(state: CoordinationTaskState) -> &'static str {
    match state {
        CoordinationTaskState::Proposed => "proposed",
        CoordinationTaskState::Ready => "ready",
        CoordinationTaskState::Running => "running",
        CoordinationTaskState::Blocked => "blocked",
        CoordinationTaskState::AwaitingReview => "awaiting_review",
        CoordinationTaskState::Completed => "completed",
        CoordinationTaskState::Failed => "failed",
        CoordinationTaskState::Cancelled => "cancelled",
    }
}

fn decode_task_state(state: &str) -> Result<CoordinationTaskState, SessionStoreError> {
    match state {
        "proposed" => Ok(CoordinationTaskState::Proposed),
        "ready" => Ok(CoordinationTaskState::Ready),
        "running" => Ok(CoordinationTaskState::Running),
        "blocked" => Ok(CoordinationTaskState::Blocked),
        "awaiting_review" => Ok(CoordinationTaskState::AwaitingReview),
        "completed" => Ok(CoordinationTaskState::Completed),
        "failed" => Ok(CoordinationTaskState::Failed),
        "cancelled" => Ok(CoordinationTaskState::Cancelled),
        _ => Err(SessionStoreError::Store(
            "stored task state is invalid".into(),
        )),
    }
}

fn checked_u32(value: i64, label: &str) -> Result<u32, SessionStoreError> {
    value
        .try_into()
        .map_err(|_| SessionStoreError::Store(format!("stored {label} is outside u32 range")))
}

const fn encode_relation_kind(kind: AgentRelationKind) -> &'static str {
    match kind {
        AgentRelationKind::ParentOf => "parent_of",
        AgentRelationKind::Peer => "peer",
        AgentRelationKind::Reviews => "reviews",
    }
}

fn decode_relation_kind(kind: &str) -> Result<AgentRelationKind, SessionStoreError> {
    match kind {
        "parent_of" => Ok(AgentRelationKind::ParentOf),
        "peer" => Ok(AgentRelationKind::Peer),
        "reviews" => Ok(AgentRelationKind::Reviews),
        _ => Err(SessionStoreError::Store(
            "stored Agent relation kind is invalid".into(),
        )),
    }
}

fn checked_i64(value: u64, label: &str) -> Result<i64, SessionStoreError> {
    value
        .try_into()
        .map_err(|_| SessionStoreError::Invalid(format!("{label} exceeds SQLite range")))
}

fn checked_u64(value: i64, label: &str) -> Result<u64, SessionStoreError> {
    value
        .try_into()
        .map_err(|_| SessionStoreError::Store(format!("stored {label} is negative")))
}
