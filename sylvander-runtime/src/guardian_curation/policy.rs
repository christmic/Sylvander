use super::models::{
    CandidateOrigin, CandidateScope, ConsentState, MemoryCandidate, MutationAction, PolicyOutcome,
    Sensitivity,
};

pub(crate) struct DeterministicGuardianPolicy {
    revision: u64,
}

impl DeterministicGuardianPolicy {
    pub(crate) fn new(revision: u64) -> Option<Self> {
        (revision > 0).then_some(Self { revision })
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn evaluate(
        &self,
        candidate: &MemoryCandidate,
        action: MutationAction,
    ) -> (PolicyOutcome, &'static str) {
        // `new` rejects revision zero, but keep the authorization boundary
        // fail-closed if construction invariants ever change.
        if self.revision == 0 {
            return (PolicyOutcome::Deny, "unsupported_policy_revision");
        }
        if candidate.evidence.is_empty() {
            return (PolicyOutcome::Deny, "evidence_required");
        }
        let Some(scope) = candidate.scope else {
            return (PolicyOutcome::Deny, "classification_required");
        };
        let Some(sensitivity) = candidate.sensitivity else {
            return (PolicyOutcome::Deny, "classification_required");
        };
        if candidate
            .retention_secs
            .is_none_or(|retention| retention == 0)
        {
            return (PolicyOutcome::Deny, "finite_retention_required");
        }
        if matches!(candidate.consent, ConsentState::Denied) {
            return (PolicyOutcome::Deny, "consent_denied");
        }

        if matches!(action, MutationAction::Forget) {
            return (PolicyOutcome::Allow, "authorized_forget");
        }
        if matches!(sensitivity, Sensitivity::Secret) {
            return (PolicyOutcome::Deny, "secret_storage_forbidden");
        }

        match scope {
            CandidateScope::UserProfile => {
                if candidate.owner_user_id.is_none() {
                    return (PolicyOutcome::Deny, "user_owner_required");
                }
                if !matches!(candidate.consent, ConsentState::Confirmed) {
                    return (PolicyOutcome::Deny, "explicit_confirmation_required");
                }
            }
            CandidateScope::Relationship => {
                if candidate.owner_user_id.is_none() {
                    return (PolicyOutcome::Deny, "user_owner_required");
                }
                if matches!(sensitivity, Sensitivity::Personal)
                    && !matches!(candidate.consent, ConsentState::Confirmed)
                {
                    return (PolicyOutcome::Deny, "personal_confirmation_required");
                }
            }
            CandidateScope::AgentCanonical => {
                if matches!(sensitivity, Sensitivity::Personal | Sensitivity::Secret) {
                    return (PolicyOutcome::Deny, "cross_user_content_forbidden");
                }
                if matches!(candidate.origin, CandidateOrigin::Inferred)
                    && candidate.confidence_basis_points.unwrap_or(0) < 8_000
                {
                    return (PolicyOutcome::Deny, "canonical_confidence_too_low");
                }
            }
            CandidateScope::WorkspaceKnowledge => {
                if candidate.workspace_id.is_none() {
                    return (PolicyOutcome::Deny, "workspace_owner_required");
                }
                if matches!(sensitivity, Sensitivity::Personal) {
                    return (PolicyOutcome::Deny, "personal_workspace_storage_forbidden");
                }
            }
        }

        (
            PolicyOutcome::Allow,
            match action {
                MutationAction::Commit => "authorized_commit",
                MutationAction::Correct => "authorized_correction",
                MutationAction::Decay => "authorized_decay",
                MutationAction::Forget => "authorized_forget",
            },
        )
    }
}
