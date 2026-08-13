//! Declarative, multi-dimensional LLM bench matrix.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchScenario {
    Connectivity,
    Usage,
    RemoteTokenCount,
    CacheWriteRead,
    OpenTimeout,
    TransientRetry,
    TruncatedStream,
    ProcessInterruption,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelBinding {
    pub model_id: String,
    /// Scenarios the selected deployment explicitly advertises.
    pub advertised_scenarios: BTreeSet<BenchScenario>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolBinding {
    pub provider_id: String,
    pub protocol: String,
    pub endpoint_origin: String,
    /// Name of the environment variable containing the credential, never its value.
    pub credential_env: String,
    pub supported_scenarios: BTreeSet<BenchScenario>,
    pub models: Vec<ModelBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchMatrix {
    pub schema_version: u32,
    pub repetitions: u32,
    pub scenarios: BTreeSet<BenchScenario>,
    pub bindings: Vec<ProtocolBinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applicability {
    Required,
    NotApplicableProtocol,
    NotApplicableModel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixCoordinate {
    pub provider_id: String,
    pub protocol: String,
    pub model_id: String,
    pub scenario: BenchScenario,
    pub run_ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatrixCell {
    pub coordinate: MatrixCoordinate,
    pub applicability: Applicability,
}

impl BenchMatrix {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != 1 {
            return Err("unsupported matrix schema version");
        }
        if self.repetitions == 0 {
            return Err("matrix repetitions must be positive");
        }
        if self.scenarios.is_empty() {
            return Err("matrix must select at least one scenario");
        }
        if self.bindings.is_empty() {
            return Err("matrix must contain at least one protocol binding");
        }

        let mut deployment_keys = BTreeSet::new();
        for binding in &self.bindings {
            if binding.provider_id.is_empty()
                || binding.protocol.is_empty()
                || binding.endpoint_origin.is_empty()
                || binding.credential_env.is_empty()
            {
                return Err("protocol binding identifiers must not be empty");
            }
            if binding.models.is_empty() {
                return Err("protocol binding must contain at least one model");
            }
            for model in &binding.models {
                if model.model_id.is_empty() {
                    return Err("model identifier must not be empty");
                }
                let key = (
                    binding.provider_id.as_str(),
                    binding.protocol.as_str(),
                    model.model_id.as_str(),
                );
                if !deployment_keys.insert(key) {
                    return Err("provider, protocol, and model coordinates must be unique");
                }
            }
        }
        Ok(())
    }

    pub fn expand(&self) -> Result<Vec<MatrixCell>, &'static str> {
        self.validate()?;
        let capacity = self
            .bindings
            .iter()
            .map(|binding| binding.models.len())
            .sum::<usize>()
            .saturating_mul(self.scenarios.len())
            .saturating_mul(usize::try_from(self.repetitions).unwrap_or(usize::MAX));
        let mut cells = Vec::with_capacity(capacity);

        for binding in &self.bindings {
            for model in &binding.models {
                for &scenario in &self.scenarios {
                    let applicability = if !binding.supported_scenarios.contains(&scenario) {
                        Applicability::NotApplicableProtocol
                    } else if !model.advertised_scenarios.contains(&scenario) {
                        Applicability::NotApplicableModel
                    } else {
                        Applicability::Required
                    };
                    for run_ordinal in 1..=self.repetitions {
                        cells.push(MatrixCell {
                            coordinate: MatrixCoordinate {
                                provider_id: binding.provider_id.clone(),
                                protocol: binding.protocol.clone(),
                                model_id: model.model_id.clone(),
                                scenario,
                                run_ordinal,
                            },
                            applicability,
                        });
                    }
                }
            }
        }
        Ok(cells)
    }
}
