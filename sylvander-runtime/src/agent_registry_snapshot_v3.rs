//! Pure, versioned data contract for multi-Provider Agent snapshots.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sylvander_protocol::ModelSelection;

/// Qualified model policy supplied when materializing one Agent revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentSnapshotSelectionV3 {
    pub agent_id: String,
    pub agent_revision: u64,
    pub default_model: ModelSelection,
    pub allowed_models: BTreeSet<ModelSelection>,
}

impl AgentSnapshotSelectionV3 {
    pub(crate) fn validate(&self) -> Result<(), AgentSnapshotV3Error> {
        validate_agent(&self.agent_id, self.agent_revision)?;
        validate_model(&self.default_model)?;
        if self.allowed_models.is_empty() {
            return Err(AgentSnapshotV3Error::EmptyModels);
        }
        for model in &self.allowed_models {
            validate_model(model)?;
        }
        if !self.allowed_models.contains(&self.default_model) {
            return Err(AgentSnapshotV3Error::DefaultNotAllowed);
        }
        Ok(())
    }
}

/// One immutable, provider-qualified Model revision pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotModelRevisionV3 {
    pub model: ModelSelection,
    pub revision: u64,
}

/// Immutable component revisions captured for one Agent revision.
///
/// `providers` is a sorted exact Provider revision map. `models` must be
/// strictly sorted by qualified identity so serialization and digests are
/// independent of database or input ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentRegistrySnapshotV3 {
    pub agent_id: String,
    pub agent_revision: u64,
    pub default_model: ModelSelection,
    pub providers: BTreeMap<String, u64>,
    pub models: Vec<SnapshotModelRevisionV3>,
}

impl AgentRegistrySnapshotV3 {
    pub(crate) fn new(
        agent_id: String,
        agent_revision: u64,
        default_model: ModelSelection,
        providers: BTreeMap<String, u64>,
        mut models: Vec<SnapshotModelRevisionV3>,
    ) -> Result<Self, AgentSnapshotV3Error> {
        models.sort_by(|left, right| left.model.cmp(&right.model));
        let snapshot = Self {
            agent_id,
            agent_revision,
            default_model,
            providers,
            models,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub(crate) fn validate(&self) -> Result<(), AgentSnapshotV3Error> {
        validate_agent(&self.agent_id, self.agent_revision)?;
        validate_model(&self.default_model)?;
        if self.providers.is_empty() {
            return Err(AgentSnapshotV3Error::EmptyProviders);
        }
        for (provider_id, revision) in &self.providers {
            if provider_id.trim().is_empty() || *revision == 0 {
                return Err(AgentSnapshotV3Error::InvalidProvider);
            }
        }
        if self.models.is_empty() {
            return Err(AgentSnapshotV3Error::EmptyModels);
        }
        for model in &self.models {
            validate_model(&model.model)?;
            if model.revision == 0 {
                return Err(AgentSnapshotV3Error::InvalidModelRevision);
            }
            if !self.providers.contains_key(&model.model.provider_id) {
                return Err(AgentSnapshotV3Error::MissingProviderPin(
                    model.model.provider_id.clone(),
                ));
            }
        }
        for pair in self.models.windows(2) {
            match pair[0].model.cmp(&pair[1].model) {
                std::cmp::Ordering::Less => {}
                std::cmp::Ordering::Equal => {
                    return Err(AgentSnapshotV3Error::DuplicateModel {
                        provider_id: pair[0].model.provider_id.clone(),
                        model_id: pair[0].model.model_id.clone(),
                    });
                }
                std::cmp::Ordering::Greater => {
                    return Err(AgentSnapshotV3Error::ModelsNotSorted);
                }
            }
        }
        if !self
            .models
            .iter()
            .any(|model| model.model == self.default_model)
        {
            return Err(AgentSnapshotV3Error::DefaultNotAllowed);
        }
        Ok(())
    }

    /// Serialize the validated, deterministically ordered contract and hash it.
    pub(crate) fn canonical_json_and_digest(
        &self,
    ) -> Result<(String, String), AgentSnapshotV3Error> {
        self.validate()?;
        let json = serde_json::to_string(self)
            .map_err(|error| AgentSnapshotV3Error::Serialization(error.to_string()))?;
        let mut hasher = Sha256::new();
        hasher.update(b"sylvander.agent-registry-snapshot/v3\0");
        hasher.update(json.as_bytes());
        let digest = format!("{:x}", hasher.finalize());
        Ok((json, digest))
    }
}

fn validate_agent(agent_id: &str, revision: u64) -> Result<(), AgentSnapshotV3Error> {
    if agent_id.trim().is_empty() || revision == 0 {
        Err(AgentSnapshotV3Error::InvalidAgent)
    } else {
        Ok(())
    }
}

fn validate_model(model: &ModelSelection) -> Result<(), AgentSnapshotV3Error> {
    if model.provider_id.trim().is_empty() || model.model_id.trim().is_empty() {
        Err(AgentSnapshotV3Error::InvalidModel)
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AgentSnapshotV3Error {
    #[error("invalid Agent snapshot identity or revision")]
    InvalidAgent,
    #[error("Agent snapshot must pin at least one Provider")]
    EmptyProviders,
    #[error("invalid Provider identity or revision")]
    InvalidProvider,
    #[error("Agent snapshot must allow at least one Model")]
    EmptyModels,
    #[error("invalid qualified Model identity")]
    InvalidModel,
    #[error("invalid Model revision")]
    InvalidModelRevision,
    #[error("Model Provider `{0}` has no exact revision pin")]
    MissingProviderPin(String),
    #[error("duplicate qualified Model `{provider_id}/{model_id}`")]
    DuplicateModel {
        provider_id: String,
        model_id: String,
    },
    #[error("qualified Models are not canonically sorted")]
    ModelsNotSorted,
    #[error("default Model is not in the allowed qualified catalog")]
    DefaultNotAllowed,
    #[error("failed to serialize Agent snapshot: {0}")]
    Serialization(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(provider_id: &str, model_id: &str) -> ModelSelection {
        ModelSelection {
            provider_id: provider_id.into(),
            model_id: model_id.into(),
        }
    }

    fn snapshot() -> AgentRegistrySnapshotV3 {
        AgentRegistrySnapshotV3::new(
            "assistant".into(),
            7,
            model("beta", "shared"),
            BTreeMap::from([("beta".into(), 3), ("alpha".into(), 2)]),
            vec![
                SnapshotModelRevisionV3 {
                    model: model("beta", "shared"),
                    revision: 5,
                },
                SnapshotModelRevisionV3 {
                    model: model("alpha", "shared"),
                    revision: 4,
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn selection_requires_a_qualified_allowed_default() {
        let valid = AgentSnapshotSelectionV3 {
            agent_id: "assistant".into(),
            agent_revision: 7,
            default_model: model("alpha", "shared"),
            allowed_models: BTreeSet::from([model("beta", "shared"), model("alpha", "shared")]),
        };
        valid.validate().unwrap();
        let mut invalid = valid.clone();
        invalid.default_model = model("missing", "shared");
        assert_eq!(
            invalid.validate(),
            Err(AgentSnapshotV3Error::DefaultNotAllowed)
        );
        invalid = valid;
        invalid.allowed_models.clear();
        assert_eq!(invalid.validate(), Err(AgentSnapshotV3Error::EmptyModels));
    }

    #[test]
    fn snapshot_sorts_qualified_models_and_hashes_stably() {
        let first = snapshot();
        assert_eq!(first.models[0].model, model("alpha", "shared"));
        let second = AgentRegistrySnapshotV3::new(
            "assistant".into(),
            7,
            model("beta", "shared"),
            BTreeMap::from([("alpha".into(), 2), ("beta".into(), 3)]),
            first.models.iter().cloned().rev().collect(),
        )
        .unwrap();
        let (first_json, first_digest) = first.canonical_json_and_digest().unwrap();
        let (second_json, second_digest) = second.canonical_json_and_digest().unwrap();
        assert_eq!(first_json, second_json);
        assert_eq!(first_digest, second_digest);
        assert_eq!(first_digest.len(), 64);
    }

    #[test]
    fn snapshot_fails_closed_for_invalid_or_ambiguous_bindings() {
        let valid = snapshot();
        let mut cases = Vec::new();
        let mut invalid = valid.clone();
        invalid.agent_revision = 0;
        cases.push(invalid);
        let mut invalid = valid.clone();
        invalid.providers.insert("alpha".into(), 0);
        cases.push(invalid);
        let mut invalid = valid.clone();
        invalid.models[0].revision = 0;
        cases.push(invalid);
        let mut invalid = valid.clone();
        invalid.providers.remove("alpha");
        cases.push(invalid);
        let mut invalid = valid.clone();
        invalid.models.push(invalid.models[0].clone());
        invalid
            .models
            .sort_by(|left, right| left.model.cmp(&right.model));
        cases.push(invalid);
        let mut invalid = valid;
        invalid.default_model = model("alpha", "missing");
        cases.push(invalid);
        for invalid in cases {
            assert!(invalid.validate().is_err());
            assert!(invalid.canonical_json_and_digest().is_err());
        }
    }
}
