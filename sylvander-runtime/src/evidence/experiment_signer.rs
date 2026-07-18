use std::collections::BTreeSet;
use std::fmt::Write;

use sha2::{Digest, Sha256};

use super::EvidenceError;
use super::evaluation::{valid_key, valid_sha256};
use super::experiment_types::{SignedExperimentEvidence, UnsignedExperimentEvidence};

pub trait ExperimentEvidenceSigner: Send + Sync {
    fn key_id(&self) -> &str;
    fn sign(&self, message: &[u8]) -> String;
}

pub struct HmacSha256EvidenceSigner {
    key_id: String,
    key: Vec<u8>,
}

impl HmacSha256EvidenceSigner {
    pub fn new(key_id: impl Into<String>, key: Vec<u8>) -> Result<Self, EvidenceError> {
        let key_id = key_id.into();
        if !valid_key(&key_id) || !(32..=1024).contains(&key.len()) {
            return Err(EvidenceError::InvalidExperimentEvidence);
        }
        Ok(Self { key_id, key })
    }
}

impl ExperimentEvidenceSigner for HmacSha256EvidenceSigner {
    fn key_id(&self) -> &str {
        &self.key_id
    }

    fn sign(&self, message: &[u8]) -> String {
        hmac_sha256(&self.key, message)
    }
}

impl Drop for HmacSha256EvidenceSigner {
    fn drop(&mut self) {
        self.key.fill(0);
    }
}

pub fn sign_experiment_evidence(
    id: String,
    mut evidence: UnsignedExperimentEvidence,
    signer: &dyn ExperimentEvidenceSigner,
) -> Result<(SignedExperimentEvidence, String), EvidenceError> {
    validate_unsigned(&evidence)?;
    evidence
        .evaluations
        .sort_by(|left, right| left.baseline_id.cmp(&right.baseline_id));
    evidence
        .artifacts
        .sort_by(|left, right| left.locator.cmp(&right.locator));
    let canonical = serde_json::to_vec(&evidence)
        .map_err(|error| EvidenceError::Serialize(error.to_string()))?;
    let digest_sha256 = hex(&Sha256::digest(&canonical));
    let signature_hex = signer.sign(&canonical);
    if !valid_key(signer.key_id()) || !valid_sha256(&signature_hex) {
        return Err(EvidenceError::InvalidExperimentEvidence);
    }
    Ok((
        SignedExperimentEvidence {
            id,
            evidence,
            digest_sha256,
            signer_key_id: signer.key_id().to_string(),
            signature_hex: signature_hex.clone(),
        },
        String::from_utf8(canonical)
            .map_err(|error| EvidenceError::Serialize(error.to_string()))?,
    ))
}

pub fn verify_experiment_evidence(
    signed: &SignedExperimentEvidence,
    verifier: &dyn ExperimentEvidenceSigner,
) -> Result<bool, EvidenceError> {
    if verifier.key_id() != signed.signer_key_id {
        return Ok(false);
    }
    let (expected, _) =
        sign_experiment_evidence(signed.id.clone(), signed.evidence.clone(), verifier)?;
    Ok(expected.digest_sha256 == signed.digest_sha256
        && expected.signature_hex == signed.signature_hex)
}

fn validate_unsigned(evidence: &UnsignedExperimentEvidence) -> Result<(), EvidenceError> {
    if !valid_key(&evidence.experiment_id)
        || !valid_sha256(&evidence.proposal_digest_sha256)
        || !valid_git_commit(&evidence.workspace_commit)
        || evidence.evaluations.is_empty()
        || evidence.evaluations.len() > 32
        || evidence.artifacts.len() > 64
        || evidence.recorded_at < 0
    {
        return Err(EvidenceError::InvalidExperimentEvidence);
    }
    let mut baselines = BTreeSet::new();
    for evaluation in &evidence.evaluations {
        if !valid_key(&evaluation.baseline_id)
            || !valid_sha256(&evaluation.baseline_digest_sha256)
            || !baselines.insert(&evaluation.baseline_id)
        {
            return Err(EvidenceError::InvalidExperimentEvidence);
        }
    }
    if evidence.artifacts.iter().any(|reference| {
        reference.locator.is_empty()
            || reference.locator.len() > 1024
            || !reference.digest_sha256.as_deref().is_some_and(valid_sha256)
    }) {
        return Err(EvidenceError::InvalidExperimentEvidence);
    }
    Ok(())
}

pub(super) fn valid_git_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> String {
    const BLOCK_SIZE: usize = 64;
    let mut block = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK_SIZE];
    let mut outer_pad = [0x5c_u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= block[index];
        outer_pad[index] ^= block[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    hex(&outer.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
#[path = "../../tests/unit/evidence_experiment_signer.rs"]
mod tests;
