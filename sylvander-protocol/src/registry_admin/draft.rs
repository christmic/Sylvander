use serde::{Deserialize, Serialize};

/// Write-only credential locator. Secret values are never accepted by this contract.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialSecretReferenceDraft {
    Environment { name: String },
    File { path: String },
}

impl CredentialSecretReferenceDraft {
    pub(super) fn is_configured(&self) -> bool {
        match self {
            Self::Environment { name } => !name.trim().is_empty(),
            Self::File { path } => !path.trim().is_empty(),
        }
    }
}

impl std::fmt::Debug for CredentialSecretReferenceDraft {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialSecretReferenceDraft([REDACTED])")
    }
}
