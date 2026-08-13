//! Versioned, content-addressed corpus manifests for paired Runtime evidence.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{CognitionProfile, RuntimeBenchResult, ScenarioFamily};

pub const CORPUS_MANIFEST_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusModality {
    Text,
    Image,
    Audio,
    Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusRisk {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusArtifact {
    /// Repository-relative or harness-owned locator. The manifest never embeds
    /// user media or credentials.
    pub locator: String,
    pub media_type: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusVerifier {
    pub id: String,
    pub revision: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusProvenance {
    pub dataset: String,
    pub revision: String,
    pub license: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusScenario {
    pub id: String,
    pub family: ScenarioFamily,
    pub modality: CorpusModality,
    pub risk: CorpusRisk,
    pub input: CorpusArtifact,
    pub verifier: CorpusVerifier,
    pub provenance: CorpusProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusManifest {
    pub schema_version: u16,
    pub suite: String,
    pub suite_revision: String,
    pub candidate: CognitionProfile,
    pub repetitions: u32,
    pub scenarios: Vec<CorpusScenario>,
}

impl CorpusManifest {
    pub fn from_json(bytes: &[u8]) -> Result<Self, CorpusManifestError> {
        let manifest = serde_json::from_slice(bytes).map_err(CorpusManifestError::Json)?;
        Self::validate_and_return(manifest)
    }

    fn validate_and_return(manifest: Self) -> Result<Self, CorpusManifestError> {
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), CorpusManifestError> {
        if self.schema_version != CORPUS_MANIFEST_SCHEMA_VERSION {
            return Err(CorpusManifestError::UnsupportedSchema);
        }
        if self.suite.trim().is_empty()
            || self.suite_revision.trim().is_empty()
            || self.repetitions == 0
            || self.scenarios.is_empty()
            || self.candidate == CognitionProfile::PrimaryOnly
        {
            return Err(CorpusManifestError::IncompleteManifest);
        }
        let mut prior = None;
        for scenario in &self.scenarios {
            validate_scenario(scenario, self.candidate)?;
            if prior.is_some_and(|id: &str| id >= scenario.id.as_str()) {
                return Err(CorpusManifestError::UnsortedOrDuplicateScenario);
            }
            prior = Some(scenario.id.as_str());
        }
        Ok(())
    }

    pub fn canonical_json_and_sha256(&self) -> Result<(String, String), CorpusManifestError> {
        self.validate()?;
        let json = serde_json::to_string(self).map_err(CorpusManifestError::Json)?;
        let digest = format!("{:x}", Sha256::digest(json.as_bytes()));
        Ok((json, digest))
    }

    /// Require every declared scenario/run exactly once in each paired arm.
    pub fn validate_pair_coverage(
        &self,
        baseline: &[RuntimeBenchResult],
        candidate: &[RuntimeBenchResult],
    ) -> Result<(), CorpusManifestError> {
        self.validate()?;
        let expected = self.expected_cells();
        let baseline = self.cells(baseline, CognitionProfile::PrimaryOnly)?;
        let candidate = self.cells(candidate, self.candidate)?;
        if baseline != expected || candidate != expected {
            return Err(CorpusManifestError::IncompletePairCoverage);
        }
        Ok(())
    }

    fn expected_cells(&self) -> BTreeSet<(String, u32)> {
        self.scenarios
            .iter()
            .flat_map(|scenario| (1..=self.repetitions).map(|run| (scenario.id.clone(), run)))
            .collect()
    }

    fn cells(
        &self,
        results: &[RuntimeBenchResult],
        profile: CognitionProfile,
    ) -> Result<BTreeSet<(String, u32)>, CorpusManifestError> {
        let scenarios = self
            .scenarios
            .iter()
            .map(|scenario| (scenario.id.as_str(), scenario.family))
            .collect::<BTreeMap<_, _>>();
        let mut cells = BTreeSet::new();
        for result in results {
            result
                .validate()
                .map_err(|_| CorpusManifestError::InvalidResult)?;
            let coordinate = &result.coordinate;
            if coordinate.suite != self.suite
                || coordinate.suite_revision != self.suite_revision
                || coordinate.cognition != profile
                || scenarios.get(coordinate.scenario_id.as_str()) != Some(&coordinate.family)
                || !cells.insert((coordinate.scenario_id.clone(), coordinate.run_ordinal))
            {
                return Err(CorpusManifestError::UnexpectedResult);
            }
        }
        Ok(cells)
    }
}

fn validate_scenario(
    scenario: &CorpusScenario,
    candidate: CognitionProfile,
) -> Result<(), CorpusManifestError> {
    let supported_family = matches!(
        scenario.family,
        ScenarioFamily::CognitiveRouting | ScenarioFamily::MultimodalPerception
    );
    let perception_modality = scenario.modality != CorpusModality::Text;
    if scenario.id.trim().is_empty()
        || scenario.input.locator.trim().is_empty()
        || scenario.input.media_type.trim().is_empty()
        || scenario.verifier.id.trim().is_empty()
        || scenario.verifier.revision.trim().is_empty()
        || scenario.provenance.dataset.trim().is_empty()
        || scenario.provenance.revision.trim().is_empty()
        || scenario.provenance.license.trim().is_empty()
        || !valid_sha256(&scenario.input.sha256)
        || !valid_sha256(&scenario.verifier.sha256)
        || !supported_family
        || (candidate == CognitionProfile::PerceptionSpecialist && !perception_modality)
        || (scenario.family == ScenarioFamily::MultimodalPerception && !perception_modality)
    {
        return Err(CorpusManifestError::InvalidScenario);
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[derive(Debug, thiserror::Error)]
pub enum CorpusManifestError {
    #[error("corpus manifest JSON is invalid: {0}")]
    Json(serde_json::Error),
    #[error("corpus manifest schema is unsupported")]
    UnsupportedSchema,
    #[error("corpus manifest is incomplete")]
    IncompleteManifest,
    #[error("corpus scenario is invalid")]
    InvalidScenario,
    #[error("corpus scenarios must be uniquely sorted by id")]
    UnsortedOrDuplicateScenario,
    #[error("benchmark result is invalid")]
    InvalidResult,
    #[error("benchmark result is outside the corpus manifest")]
    UnexpectedResult,
    #[error("paired benchmark coverage is incomplete")]
    IncompletePairCoverage,
}

#[cfg(test)]
#[path = "../tests/unit/corpus.rs"]
mod tests;
