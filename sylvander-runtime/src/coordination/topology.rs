//! Validated relationship graph for first-class Agent instances.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use sylvander_api::{AgentInstanceId, SessionId};

use crate::session::membership::SessionMembership;

/// A durable relationship between two Agent instances.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRelationKind {
    /// `source` owns and governs work delegated to `target`.
    ParentOf,
    /// Symmetric collaboration without lifecycle ownership.
    Peer,
    /// `source` reviews outcomes produced by `target`.
    Reviews,
}

/// One edge in the Session Agent graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRelation {
    pub source: AgentInstanceId,
    pub target: AgentInstanceId,
    pub kind: AgentRelationKind,
    pub created_at: i64,
}

/// Complete topology synchronized to an exact membership snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTopology {
    pub session_id: SessionId,
    pub membership_revision: u64,
    pub topology_revision: u64,
    pub relations: Vec<AgentRelation>,
    pub updated_at: i64,
}

impl SessionTopology {
    pub fn new(
        session_id: SessionId,
        membership_revision: u64,
        topology_revision: u64,
        relations: Vec<AgentRelation>,
        updated_at: i64,
        membership: &SessionMembership,
    ) -> Result<Self, TopologyError> {
        let topology = Self {
            session_id,
            membership_revision,
            topology_revision,
            relations,
            updated_at,
        };
        topology.validate(membership)?;
        Ok(topology)
    }

    /// Validate membership synchronization and the moderator-rooted ownership tree.
    pub fn validate(&self, membership: &SessionMembership) -> Result<(), TopologyError> {
        if self.session_id != membership.session_id {
            return Err(TopologyError::SessionMismatch);
        }
        if self.membership_revision != membership.governance.membership_revision {
            return Err(TopologyError::StaleMembership {
                topology: self.membership_revision,
                membership: membership.governance.membership_revision,
            });
        }

        let members: HashSet<_> = membership
            .participants
            .iter()
            .map(|participant| participant.instance_id.clone())
            .collect();
        let moderator = &membership.governance.moderator_instance_id;
        let mut unique_edges = HashSet::with_capacity(self.relations.len());
        let mut parent_by_child = HashMap::new();
        let mut children_by_parent: HashMap<AgentInstanceId, Vec<AgentInstanceId>> = HashMap::new();

        for relation in &self.relations {
            if !members.contains(&relation.source) {
                return Err(TopologyError::UnknownInstance(relation.source.clone()));
            }
            if !members.contains(&relation.target) {
                return Err(TopologyError::UnknownInstance(relation.target.clone()));
            }
            if relation.source == relation.target {
                return Err(TopologyError::SelfRelation(relation.source.clone()));
            }
            let edge_key = relation_key(relation);
            if !unique_edges.insert(edge_key) {
                return Err(TopologyError::DuplicateRelation);
            }
            if relation.kind != AgentRelationKind::ParentOf {
                continue;
            }
            if &relation.target == moderator {
                return Err(TopologyError::ModeratorHasParent);
            }
            if parent_by_child
                .insert(relation.target.clone(), relation.source.clone())
                .is_some()
            {
                return Err(TopologyError::MultipleParents(relation.target.clone()));
            }
            children_by_parent
                .entry(relation.source.clone())
                .or_default()
                .push(relation.target.clone());
        }

        let mut reachable = HashSet::with_capacity(members.len());
        let mut queue = VecDeque::from([moderator.clone()]);
        while let Some(current) = queue.pop_front() {
            if !reachable.insert(current.clone()) {
                continue;
            }
            if let Some(children) = children_by_parent.get(&current) {
                queue.extend(children.iter().cloned());
            }
        }
        if reachable.len() != members.len() {
            let instance = members
                .into_iter()
                .find(|member| !reachable.contains(member))
                .expect("different set sizes guarantee an unreachable member");
            if parent_cycle_from(&instance, &parent_by_child) {
                return Err(TopologyError::OwnershipCycle(instance));
            }
            return Err(TopologyError::UnreachableFromModerator(instance));
        }
        Ok(())
    }

    /// Select the nearest common owner able to arbitrate between two Agents.
    #[must_use]
    pub fn arbitrator_for(
        &self,
        source: &AgentInstanceId,
        target: &AgentInstanceId,
        membership: &SessionMembership,
    ) -> AgentInstanceId {
        let parents: HashMap<_, _> = self
            .relations
            .iter()
            .filter(|relation| relation.kind == AgentRelationKind::ParentOf)
            .map(|relation| (relation.target.clone(), relation.source.clone()))
            .collect();
        let mut source_ancestors = HashSet::new();
        let mut cursor = Some(source);
        while let Some(instance) = cursor {
            source_ancestors.insert(instance.clone());
            cursor = parents.get(instance);
        }
        let mut cursor = Some(target);
        while let Some(instance) = cursor {
            if source_ancestors.contains(instance) {
                return instance.clone();
            }
            cursor = parents.get(instance);
        }
        membership.governance.moderator_instance_id.clone()
    }
}

fn relation_key(relation: &AgentRelation) -> (AgentRelationKind, String, String) {
    let mut source = relation.source.0.clone();
    let mut target = relation.target.0.clone();
    if relation.kind == AgentRelationKind::Peer && source > target {
        std::mem::swap(&mut source, &mut target);
    }
    (relation.kind, source, target)
}

fn parent_cycle_from(
    start: &AgentInstanceId,
    parent_by_child: &HashMap<AgentInstanceId, AgentInstanceId>,
) -> bool {
    let mut visited = HashSet::new();
    let mut current = start;
    while let Some(parent) = parent_by_child.get(current) {
        if !visited.insert(current.clone()) {
            return true;
        }
        current = parent;
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopologyError {
    #[error("topology and membership belong to different Sessions")]
    SessionMismatch,
    #[error("topology membership revision {topology} is stale; current revision is {membership}")]
    StaleMembership { topology: u64, membership: u64 },
    #[error("topology references unknown Agent instance {0}")]
    UnknownInstance(AgentInstanceId),
    #[error("Agent instance {0} cannot relate to itself")]
    SelfRelation(AgentInstanceId),
    #[error("topology contains a duplicate relationship")]
    DuplicateRelation,
    #[error("the Session moderator cannot have a hierarchy parent")]
    ModeratorHasParent,
    #[error("Agent instance {0} has multiple hierarchy parents")]
    MultipleParents(AgentInstanceId),
    #[error("Agent ownership contains a cycle involving {0}")]
    OwnershipCycle(AgentInstanceId),
    #[error("Agent instance {0} is not governed by the Session moderator")]
    UnreachableFromModerator(AgentInstanceId),
}

#[cfg(test)]
#[path = "../../tests/unit/session_topology.rs"]
mod tests;
