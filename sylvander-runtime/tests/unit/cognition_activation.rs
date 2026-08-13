use std::collections::BTreeSet;

use tempfile::tempdir;

use super::*;
use crate::agent::cognition::CognitiveRoleBinding;
use crate::config::ServerConfig;
use crate::registry::snapshot::AgentSnapshotSelectionV3;
use sylvander_api::{AuthenticationMethod, PrincipalId, PrincipalKind};

fn config() -> ServerConfig {
    let mut config =
        ServerConfig::from_toml(include_str!("../../../config/sylvander.example.toml")).unwrap();
    let agent = config.agents.first_mut().unwrap();
    agent.spec.cognition.roles.push(CognitiveRoleBinding {
        role: CognitiveRole::Vision,
        model: ModelSelection {
            provider_id: "primary".into(),
            model_id: "claude-sonnet".into(),
        },
    });
    config
}

fn administrator(id: &str) -> AuthenticatedPrincipal {
    AuthenticatedPrincipal {
        id: PrincipalId::new(id),
        kind: PrincipalKind::System,
        authentication: AuthenticationMethod::Internal,
        roles: Vec::new(),
    }
}

fn evidence() -> CognitionActivationEvidence {
    CognitionActivationEvidence {
        evidence_set_sha256: "a".repeat(64),
        pairs: 20,
        minimum_pairs: 20,
        unsafe_candidates: 0,
        median_reward_gain_micros: 20_000,
        minimum_reward_gain_micros: 10_000,
        quality_win_basis_points: 7_000,
        minimum_quality_win_basis_points: 6_000,
        median_token_increase_basis_points: 500,
        maximum_token_increase_basis_points: 1_000,
        p95_latency_increase_basis_points: 800,
        maximum_p95_latency_increase_basis_points: 1_000,
    }
}

async fn seeded(path: &std::path::Path) -> (AgentRegistry, CognitionActivationDraft) {
    let config = config();
    let registry = AgentRegistry::open(path).await.unwrap();
    registry.bootstrap_registries(&config).await.unwrap();
    registry.seed(&config).await.unwrap();
    let definition = config.agents[0].clone();
    let model = ModelSelection {
        provider_id: "primary".into(),
        model_id: "claude-sonnet".into(),
    };
    registry
        .stage_agent_snapshot_v3(AgentSnapshotSelectionV3 {
            agent_id: definition.spec.id.0.clone(),
            agent_revision: definition.revision,
            default_model: model.clone(),
            allowed_models: BTreeSet::from([model.clone()]),
        })
        .await
        .unwrap();
    let stored = registry
        .load(&definition.spec.id, definition.revision)
        .await
        .unwrap()
        .unwrap();
    let draft = CognitionActivationDraft {
        agent_id: definition.spec.id,
        agent_revision: definition.revision,
        agent_definition_sha256: stored.digest,
        role: CognitiveRole::Vision,
        model,
        evidence: evidence(),
    };
    (registry, draft)
}

#[tokio::test]
async fn activation_requires_eligible_evidence_and_an_administrator() {
    let directory = tempdir().unwrap();
    let (registry, draft) = seeded(&directory.path().join("registry.db")).await;
    assert!(matches!(
        registry
            .propose_cognition_activation(None, draft.clone())
            .await,
        Err(CognitionActivationError::Unauthorized)
    ));
    let mut ineligible = draft;
    ineligible.evidence.unsafe_candidates = 1;
    assert!(matches!(
        registry
            .propose_cognition_activation(Some(&administrator("owner")), ineligible)
            .await,
        Err(CognitionActivationError::IneligibleEvidence)
    ));
}

#[tokio::test]
async fn approval_is_exactly_bound_revocable_and_durable_across_restart() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("registry.db");
    let (registry, draft) = seeded(&path).await;
    let owner = administrator("owner");
    let proposed = registry
        .propose_cognition_activation(Some(&owner), draft.clone())
        .await
        .unwrap();
    assert!(matches!(
        registry
            .approve_cognition_activation(Some(&owner), &proposed.proposal_id, 9)
            .await,
        Err(CognitionActivationError::Conflict)
    ));
    let approved = registry
        .approve_cognition_activation(Some(&owner), &proposed.proposal_id, 1)
        .await
        .unwrap();
    assert_eq!(approved.state, CognitionActivationState::Approved);
    drop(registry);

    let registry = AgentRegistry::open(&path).await.unwrap();
    let active = registry
        .active_cognition_activation(&draft.agent_id, draft.agent_revision, draft.role)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(active.proposal_id, proposed.proposal_id);
    registry
        .revoke_cognition_activation(Some(&owner), &active.proposal_id, 2)
        .await
        .unwrap();
    assert!(
        registry
            .active_cognition_activation(&draft.agent_id, draft.agent_revision, draft.role)
            .await
            .unwrap()
            .is_none()
    );
}
