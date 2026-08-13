use serde::{Deserialize, Serialize};

use crate::{FailurePoint, RuntimeBenchCoordinate};

/// One deterministic interruption selected by the harness supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultInjectionSpec {
    pub point: FailurePoint,
    pub occurrence: u32,
}

impl FaultInjectionSpec {
    pub fn validate(self) -> Result<(), FaultInjectionError> {
        if self.point == FailurePoint::None || self.occurrence == 0 {
            return Err(FaultInjectionError::InvalidSpec);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FaultReceipt {
    pub point: FailurePoint,
    pub occurrence: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultDecision {
    Continue,
    Interrupt(FaultReceipt),
}

/// Deterministic controller kept by the harness supervisor, outside the
/// Runtime process being interrupted.
#[derive(Debug, Clone)]
pub struct FaultController {
    spec: FaultInjectionSpec,
    observations: u32,
    fired: bool,
}

impl FaultController {
    pub fn new(spec: FaultInjectionSpec) -> Result<Self, FaultInjectionError> {
        spec.validate()?;
        Ok(Self {
            spec,
            observations: 0,
            fired: false,
        })
    }

    pub fn for_coordinate(
        coordinate: &RuntimeBenchCoordinate,
        occurrence: u32,
    ) -> Result<Self, FaultInjectionError> {
        coordinate
            .validate()
            .map_err(|_| FaultInjectionError::InvalidCoordinate)?;
        Self::new(FaultInjectionSpec {
            point: coordinate.failure_point,
            occurrence,
        })
    }

    #[must_use]
    pub fn checkpoint(&mut self, point: FailurePoint) -> FaultDecision {
        if self.fired || point != self.spec.point {
            return FaultDecision::Continue;
        }
        self.observations = self.observations.saturating_add(1);
        if self.observations != self.spec.occurrence {
            return FaultDecision::Continue;
        }
        self.fired = true;
        FaultDecision::Interrupt(FaultReceipt {
            point,
            occurrence: self.observations,
        })
    }

    #[must_use]
    pub const fn fired(&self) -> bool {
        self.fired
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FaultInjectionError {
    #[error("fault injection requires a concrete point and positive occurrence")]
    InvalidSpec,
    #[error("fault injection coordinate is invalid")]
    InvalidCoordinate,
}
