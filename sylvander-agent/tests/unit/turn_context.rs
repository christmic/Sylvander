use super::*;

use crate::tools::memory::{InMemoryMemoryStore, MemoryAppend, MemoryKind};
use crate::workspace_executor::LocalExecutor;

fn provenance(source: TurnContextSource, reference: &str) -> TurnContextProvenance {
    TurnContextProvenance::new(source, reference)
}

#[test]
fn prompt_resolver_maps_model_profile_and_agent_definition_into_one_typed_layer() {
    let selection = sylvander_protocol::ModelSelection {
        provider_id: "provider".into(),
        model_id: "model".into(),
    };
    let resolver = crate::prompt::PromptResolver::new(
        "agent:test@5".into(),
        "agent instructions".into(),
        vec![crate::prompt::PromptProfile {
            id: "coding".into(),
            qualified_models: vec![selection.clone()],
            providers: Vec::new(),
            models: Vec::new(),
            system_prompt: "model-specific instructions".into(),
        }],
        Some("coding".into()),
        true,
    )
    .unwrap();
    let inputs = resolver
        .turn_context_inputs(&selection, None, Some("session override"), None)
        .unwrap();
    let composed = compose_turn_context(inputs, &TurnContextBudgets::default(), 1).unwrap();
    let agent = composed
        .manifest
        .layers
        .iter()
        .find(|layer| layer.kind == TurnContextLayerKind::Agent)
        .unwrap();
    assert_eq!(
        agent
            .included_items
            .iter()
            .map(|item| item.provenance.source)
            .collect::<Vec<_>>(),
        vec![
            TurnContextSource::ModelProfile,
            TurnContextSource::AgentDefinition
        ]
    );
    assert_eq!(
        composed.manifest.layers.last().unwrap().kind,
        TurnContextLayerKind::Session
    );
}

#[test]
fn composition_is_typed_ordered_digested_and_budgeted() {
    let mut inputs = TurnContextInputs::default();
    inputs.push_required(
        TurnContextLayerKind::Safety,
        TurnContextCandidate::authoritative(
            "safety policy",
            provenance(TurnContextSource::RuntimeSafety, "safety:v1"),
        ),
    );
    inputs.push_required(
        TurnContextLayerKind::Agent,
        TurnContextCandidate::authoritative(
            "agent instructions",
            provenance(TurnContextSource::AgentDefinition, "agent:test@4").with_revision(4),
        ),
    );
    inputs.push_required(
        TurnContextLayerKind::UserProfile,
        TurnContextCandidate::authoritative(
            "user profile",
            provenance(TurnContextSource::UserProfile, "user-profile").with_revision(7),
        ),
    );
    inputs.extend_retrieved(
        TurnContextLayerKind::RelationshipMemory,
        [TurnContextCandidate::retrieved(
            "prefers concise explanations",
            provenance(TurnContextSource::RelationshipMemory, "relationship:m1").with_revision(2),
            30,
        )],
    );
    inputs.extend_retrieved(
        TurnContextLayerKind::WorkspaceKnowledge,
        [TurnContextCandidate::retrieved(
            "src/lib.rs:4: use the typed boundary",
            provenance(TurnContextSource::WorkspaceSearch, "local:src/lib.rs#4"),
            20,
        )],
    );
    inputs.push_required(
        TurnContextLayerKind::Session,
        TurnContextCandidate::authoritative(
            "answer this turn in Chinese",
            provenance(TurnContextSource::SessionOverride, "session:s1"),
        ),
    );

    let composed = compose_turn_context(inputs, &TurnContextBudgets::default(), 100).unwrap();
    let kinds = composed
        .manifest
        .layers
        .iter()
        .map(|layer| layer.kind)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![
            TurnContextLayerKind::Safety,
            TurnContextLayerKind::Agent,
            TurnContextLayerKind::UserProfile,
            TurnContextLayerKind::RelationshipMemory,
            TurnContextLayerKind::WorkspaceKnowledge,
            TurnContextLayerKind::Session,
        ]
    );
    for pair in composed.manifest.layers.windows(2) {
        assert!(pair[0].precedence < pair[1].precedence);
    }
    assert_eq!(
        composed.manifest.total_bytes,
        composed.system_prompt().len()
    );
    assert_eq!(composed.manifest.aggregate_sha256.len(), 64);
    assert!(
        composed
            .manifest
            .layers
            .iter()
            .all(|layer| layer.byte_count <= layer.budget.max_bytes
                && layer.estimated_tokens <= layer.budget.max_estimated_tokens)
    );
    assert!(
        composed.system_prompt().find("safety policy").unwrap()
            < composed.system_prompt().find("agent instructions").unwrap()
    );
    assert!(
        composed.system_prompt().find("src/lib.rs:4").unwrap()
            < composed
                .system_prompt()
                .find("answer this turn in Chinese")
                .unwrap()
    );
}

#[test]
fn retrieved_items_are_ranked_bounded_and_inactive_items_are_excluded() {
    let mut inputs = TurnContextInputs::default();
    inputs.extend_retrieved(
        TurnContextLayerKind::RelationshipMemory,
        [
            TurnContextCandidate::retrieved(
                "expired secret",
                provenance(TurnContextSource::RelationshipMemory, "expired"),
                100,
            )
            .with_expiry(Some(99)),
            TurnContextCandidate::retrieved(
                "superseded preference",
                provenance(TurnContextSource::RelationshipMemory, "old"),
                90,
            )
            .with_superseded_by(Some("new".into())),
            TurnContextCandidate::retrieved(
                "most relevant active",
                provenance(TurnContextSource::RelationshipMemory, "active-high"),
                80,
            ),
            TurnContextCandidate::retrieved(
                "less relevant active",
                provenance(TurnContextSource::RelationshipMemory, "active-low"),
                20,
            ),
        ],
    );
    let mut budgets = TurnContextBudgets::default();
    budgets.relationship_memory.max_items = 1;

    let composed = compose_turn_context(inputs, &budgets, 100).unwrap();
    let layer = &composed.manifest.layers[0];
    assert_eq!(layer.kind, TurnContextLayerKind::RelationshipMemory);
    assert_eq!(layer.included_items.len(), 1);
    assert_eq!(layer.included_items[0].provenance.reference, "active-high");
    assert_eq!(layer.omitted_items, 3);
    assert!(!composed.system_prompt().contains("expired secret"));
    assert!(!composed.system_prompt().contains("superseded preference"));
    assert!(!composed.system_prompt().contains("less relevant active"));
}

#[test]
fn required_layer_fails_closed_instead_of_silently_truncating() {
    let mut inputs = TurnContextInputs::default();
    inputs.push_required(
        TurnContextLayerKind::Agent,
        TurnContextCandidate::authoritative(
            "x".repeat(200),
            provenance(TurnContextSource::AgentDefinition, "agent:test"),
        ),
    );
    let budgets = TurnContextBudgets {
        agent: TurnContextBudget::new(100, 100, 1),
        ..TurnContextBudgets::default()
    };
    assert_eq!(
        compose_turn_context(inputs, &budgets, 100),
        Err(TurnContextError::RequiredLayerBudgetExceeded)
    );
}

#[tokio::test]
async fn relationship_retrieval_uses_query_and_never_returns_superseded_heads() {
    let store = InMemoryMemoryStore::new();
    let session = sylvander_protocol::SessionContext::new("user", "agent", "session");
    let context = MemoryExecutionContext::application_worker(&session);
    let relevant = store
        .append_relationship(
            &context,
            MemoryAppend::new("Rust workspace uses cargo nextest")
                .with_kind(MemoryKind::ProjectFact),
        )
        .await
        .unwrap();
    store
        .append_relationship(
            &context,
            MemoryAppend::new("unrelated cooking preference").with_kind(MemoryKind::Preference),
        )
        .await
        .unwrap();
    store
        .supersede_relationship(
            &context,
            &relevant.id,
            relevant.revision,
            MemoryAppend::new("Rust workspace uses cargo test").with_kind(MemoryKind::ProjectFact),
        )
        .await
        .unwrap();

    let results = retrieve_relationship_context(
        &store,
        &context,
        "please inspect the Rust workspace",
        TurnContextBudgets::default().relationship_memory,
        crate::session::now_secs(),
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].content().contains("cargo test"));
    assert!(!results[0].content().contains("nextest"));
    assert!(results[0].provenance.reference.starts_with("relationship:"));
}

#[tokio::test]
async fn workspace_retrieval_returns_only_matching_bounded_lines() {
    let workspace = tempfile::TempDir::new().unwrap();
    std::fs::write(
        workspace.path().join("ARCHITECTURE.md"),
        "Session routing is server authoritative.\nUnrelated line.\n",
    )
    .unwrap();
    std::fs::write(
        workspace.path().join("notes.md"),
        "server authoritative model selection\n",
    )
    .unwrap();
    let target = WorkspaceTarget::local(workspace.path(), true);
    let mut budget = TurnContextBudgets::default().workspace_knowledge;
    budget.max_items = 2;

    let results = retrieve_workspace_context(
        &LocalExecutor,
        &target,
        "explain authoritative session routing",
        budget,
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|item| item.content().contains("authoritative"))
    );

    let mut inputs = TurnContextInputs::default();
    inputs.extend_retrieved(TurnContextLayerKind::WorkspaceKnowledge, results);
    let mut budgets = TurnContextBudgets::default();
    budgets.workspace_knowledge.max_items = 1;
    let composed = compose_turn_context(inputs, &budgets, crate::session::now_secs()).unwrap();
    assert_eq!(composed.manifest.layers[0].included_items.len(), 1);
    assert_eq!(composed.manifest.layers[0].omitted_items, 1);
}
