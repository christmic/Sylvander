//! Versioned, content-addressed corpus manifests for paired Runtime evidence.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    BenchmarkModelBinding, BenchmarkModelRole, CognitionProfile, FailurePoint,
    RuntimeBenchCoordinate, RuntimeBenchPlan, RuntimeBenchResult, ScenarioFamily, TopologyProfile,
    WorkspaceProfile,
};

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
    pub primary_model: String,
    /// Auxiliary bindings only, in canonical role order. The primary binding
    /// is inherited exactly by both paired arms.
    pub auxiliary_models: Vec<BenchmarkModelBinding>,
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
            || self.primary_model.trim().is_empty()
            || self.auxiliary_models.is_empty()
        {
            return Err(CorpusManifestError::IncompleteManifest);
        }
        validate_auxiliary_models(self.candidate, &self.auxiliary_models)?;
        let mut prior = None;
        for scenario in &self.scenarios {
            validate_scenario(scenario, self.candidate, &self.auxiliary_models)?;
            if prior.is_some_and(|id: &str| id >= scenario.id.as_str()) {
                return Err(CorpusManifestError::UnsortedOrDuplicateScenario);
            }
            prior = Some(scenario.id.as_str());
        }
        Ok(())
    }

    pub fn paired_plans(
        &self,
    ) -> Result<(RuntimeBenchPlan, RuntimeBenchPlan), CorpusManifestError> {
        self.validate()?;
        let primary = BenchmarkModelBinding {
            role: BenchmarkModelRole::Primary,
            model: self.primary_model.clone(),
        };
        let mut baseline = Vec::new();
        let mut candidate = Vec::new();
        for scenario in &self.scenarios {
            for run_ordinal in 1..=self.repetitions {
                let coordinate = |cognition, models| RuntimeBenchCoordinate {
                    suite: self.suite.clone(),
                    suite_revision: self.suite_revision.clone(),
                    scenario_id: scenario.id.clone(),
                    family: scenario.family,
                    topology: TopologyProfile::SingleAgent,
                    workspace: WorkspaceProfile::ReadOnlyShared,
                    failure_point: FailurePoint::None,
                    cognition,
                    models,
                    run_ordinal,
                };
                baseline.push(coordinate(
                    CognitionProfile::PrimaryOnly,
                    vec![primary.clone()],
                ));
                let mut models = Vec::with_capacity(self.auxiliary_models.len() + 1);
                models.push(primary.clone());
                models.extend(self.auxiliary_models.iter().cloned());
                candidate.push(coordinate(self.candidate, models));
            }
        }
        let baseline = RuntimeBenchPlan {
            schema_version: 2,
            coordinates: baseline,
        };
        let candidate = RuntimeBenchPlan {
            schema_version: 2,
            coordinates: candidate,
        };
        baseline
            .validate()
            .map_err(|_| CorpusManifestError::InvalidPlan)?;
        candidate
            .validate()
            .map_err(|_| CorpusManifestError::InvalidPlan)?;
        Ok((baseline, candidate))
    }

    pub fn canonical_json_and_sha256(&self) -> Result<(String, String), CorpusManifestError> {
        self.validate()?;
        let json = serde_json::to_string(self).map_err(CorpusManifestError::Json)?;
        let digest = format!("{:x}", Sha256::digest(json.as_bytes()));
        Ok((json, digest))
    }

    /// Verify every declared artifact against the directory containing the
    /// manifest. Absolute paths, traversal, symlink escape, directories, and
    /// changed bytes are rejected.
    pub fn verify_artifacts(
        &self,
        manifest_path: impl AsRef<Path>,
    ) -> Result<(), CorpusManifestError> {
        self.validate()?;
        let parent = manifest_path
            .as_ref()
            .parent()
            .ok_or(CorpusManifestError::UnsafeArtifactPath)?
            .canonicalize()
            .map_err(|_| CorpusManifestError::ArtifactUnavailable)?;
        for scenario in &self.scenarios {
            let locator = Path::new(&scenario.input.locator);
            if locator.is_absolute()
                || locator
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(CorpusManifestError::UnsafeArtifactPath);
            }
            let artifact = parent.join(locator);
            let canonical = artifact
                .canonicalize()
                .map_err(|_| CorpusManifestError::ArtifactUnavailable)?;
            if !canonical.starts_with(&parent) || !canonical.is_file() {
                return Err(CorpusManifestError::UnsafeArtifactPath);
            }
            if file_sha256(&canonical)? != scenario.input.sha256 {
                return Err(CorpusManifestError::ArtifactDigestMismatch);
            }
        }
        Ok(())
    }

    /// Require every declared scenario/run exactly once in each paired arm.
    pub fn validate_pair_coverage(
        &self,
        baseline: &[RuntimeBenchResult],
        candidate: &[RuntimeBenchResult],
    ) -> Result<(), CorpusManifestError> {
        self.validate()?;
        let (baseline_plan, candidate_plan) = self.paired_plans()?;
        let baseline = exact_coordinates(baseline)?;
        let candidate = exact_coordinates(candidate)?;
        if baseline != baseline_plan.coordinates.into_iter().collect()
            || candidate != candidate_plan.coordinates.into_iter().collect()
        {
            return Err(CorpusManifestError::IncompletePairCoverage);
        }
        Ok(())
    }
}

fn exact_coordinates(
    results: &[RuntimeBenchResult],
) -> Result<HashSet<RuntimeBenchCoordinate>, CorpusManifestError> {
    let mut coordinates = HashSet::with_capacity(results.len());
    for result in results {
        result
            .validate()
            .map_err(|_| CorpusManifestError::InvalidResult)?;
        if !coordinates.insert(result.coordinate.clone()) {
            return Err(CorpusManifestError::UnexpectedResult);
        }
    }
    Ok(coordinates)
}

fn validate_auxiliary_models(
    profile: CognitionProfile,
    bindings: &[BenchmarkModelBinding],
) -> Result<(), CorpusManifestError> {
    let allowed = match profile {
        CognitionProfile::PrimaryOnly => return Err(CorpusManifestError::InvalidModels),
        CognitionProfile::FastSlow => &[
            BenchmarkModelRole::FastDraft,
            BenchmarkModelRole::Deliberation,
        ][..],
        CognitionProfile::PrimaryCritic => &[BenchmarkModelRole::Critic][..],
        CognitionProfile::PerceptionSpecialist => &[
            BenchmarkModelRole::Vision,
            BenchmarkModelRole::Audio,
            BenchmarkModelRole::Document,
        ][..],
    };
    let mut prior = None;
    for binding in bindings {
        let rank = model_role_rank(binding.role);
        if binding.model.trim().is_empty()
            || !allowed.contains(&binding.role)
            || prior.is_some_and(|previous| previous >= rank)
        {
            return Err(CorpusManifestError::InvalidModels);
        }
        prior = Some(rank);
    }
    Ok(())
}

const fn model_role_rank(role: BenchmarkModelRole) -> u8 {
    match role {
        BenchmarkModelRole::Primary => 0,
        BenchmarkModelRole::FastDraft => 1,
        BenchmarkModelRole::Deliberation => 2,
        BenchmarkModelRole::Critic => 3,
        BenchmarkModelRole::Vision => 4,
        BenchmarkModelRole::Audio => 5,
        BenchmarkModelRole::Document => 6,
    }
}

fn validate_scenario(
    scenario: &CorpusScenario,
    candidate: CognitionProfile,
    auxiliary_models: &[BenchmarkModelBinding],
) -> Result<(), CorpusManifestError> {
    let supported_family = matches!(
        scenario.family,
        ScenarioFamily::CognitiveRouting | ScenarioFamily::MultimodalPerception
    );
    let perception_modality = scenario.modality != CorpusModality::Text;
    let required_role = match scenario.modality {
        CorpusModality::Text => None,
        CorpusModality::Image => Some(BenchmarkModelRole::Vision),
        CorpusModality::Audio => Some(BenchmarkModelRole::Audio),
        CorpusModality::Document => Some(BenchmarkModelRole::Document),
    };
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
        || (candidate != CognitionProfile::PerceptionSpecialist && perception_modality)
        || (scenario.family == ScenarioFamily::MultimodalPerception && !perception_modality)
        || required_role
            .is_some_and(|role| !auxiliary_models.iter().any(|binding| binding.role == role))
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

fn file_sha256(path: &Path) -> Result<String, CorpusManifestError> {
    let mut file = File::open(path).map_err(|_| CorpusManifestError::ArtifactUnavailable)?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| CorpusManifestError::ArtifactUnavailable)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
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
    #[error("corpus model bindings are invalid")]
    InvalidModels,
    #[error("corpus could not produce a valid paired benchmark plan")]
    InvalidPlan,
    #[error("corpus artifact path is unsafe")]
    UnsafeArtifactPath,
    #[error("corpus artifact is unavailable")]
    ArtifactUnavailable,
    #[error("corpus artifact digest does not match the manifest")]
    ArtifactDigestMismatch,
}

#[cfg(test)]
#[path = "../tests/unit/corpus.rs"]
mod tests;
