//! SWE-bench prediction artifact export.
//!
//! The wire fields follow the official SWE-bench predictions contract. Patch
//! content is an external verifier artifact and must not enter normalized
//! aggregate [`crate::result::AgentBenchResult`] records.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweBenchPrediction {
    pub instance_id: String,
    pub model_patch: String,
    pub model_name_or_path: String,
}

impl SweBenchPrediction {
    pub fn from_workspace(
        workspace: &Path,
        instance_id: impl Into<String>,
        model_name_or_path: impl Into<String>,
    ) -> Result<Self, String> {
        let instance_id = instance_id.into();
        let model_name_or_path = model_name_or_path.into();
        if instance_id.trim().is_empty() || model_name_or_path.trim().is_empty() {
            return Err("SWE-bench instance and model identifiers are required".into());
        }
        let output = Command::new("git")
            .args(["-C"])
            .arg(workspace)
            .args(["diff", "--binary", "HEAD", "--"])
            .output()
            .map_err(|error| format!("failed to run git diff: {error}"))?;
        if !output.status.success() {
            return Err("failed to export SWE-bench workspace diff".into());
        }
        let model_patch = String::from_utf8(output.stdout)
            .map_err(|_| "SWE-bench workspace diff is not UTF-8")?;
        if model_patch.is_empty() {
            return Err("SWE-bench prediction patch is empty".into());
        }
        Ok(Self {
            instance_id,
            model_patch,
            model_name_or_path,
        })
    }
}
