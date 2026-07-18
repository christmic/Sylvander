use super::*;

fn model(provider_id: &str, model_id: &str) -> ModelSelection {
    ModelSelection {
        provider_id: provider_id.into(),
        model_id: model_id.into(),
    }
}

fn snapshot() -> AgentRegistrySnapshotV3 {
    AgentRegistrySnapshotV3::new(
        "assistant".into(),
        7,
        model("beta", "shared"),
        BTreeMap::from([("beta".into(), 3), ("alpha".into(), 2)]),
        vec![
            SnapshotModelRevisionV3 {
                model: model("beta", "shared"),
                revision: 5,
            },
            SnapshotModelRevisionV3 {
                model: model("alpha", "shared"),
                revision: 4,
            },
        ],
    )
    .unwrap()
}

#[test]
fn selection_requires_a_qualified_allowed_default() {
    let valid = AgentSnapshotSelectionV3 {
        agent_id: "assistant".into(),
        agent_revision: 7,
        default_model: model("alpha", "shared"),
        allowed_models: BTreeSet::from([model("beta", "shared"), model("alpha", "shared")]),
    };
    valid.validate().unwrap();
    let mut invalid = valid.clone();
    invalid.default_model = model("missing", "shared");
    assert!(matches!(
        invalid.validate(),
        Err(AgentSnapshotV3Error::DefaultNotAllowed)
    ));
    invalid = valid;
    invalid.allowed_models.clear();
    assert!(matches!(
        invalid.validate(),
        Err(AgentSnapshotV3Error::EmptyModels)
    ));
}

#[test]
fn snapshot_sorts_qualified_models_and_hashes_stably() {
    let first = snapshot();
    assert_eq!(first.models[0].model, model("alpha", "shared"));
    let second = AgentRegistrySnapshotV3::new(
        "assistant".into(),
        7,
        model("beta", "shared"),
        BTreeMap::from([("alpha".into(), 2), ("beta".into(), 3)]),
        first.models.iter().cloned().rev().collect(),
    )
    .unwrap();
    let (first_json, first_digest) = first.canonical_json_and_digest().unwrap();
    let (second_json, second_digest) = second.canonical_json_and_digest().unwrap();
    assert_eq!(first_json, second_json);
    assert_eq!(first_digest, second_digest);
    assert_eq!(first_digest.len(), 64);
}

#[test]
fn snapshot_fails_closed_for_invalid_or_ambiguous_bindings() {
    let valid = snapshot();
    let mut cases = Vec::new();
    let mut invalid = valid.clone();
    invalid.agent_revision = 0;
    cases.push(invalid);
    let mut invalid = valid.clone();
    invalid.providers.insert("alpha".into(), 0);
    cases.push(invalid);
    let mut invalid = valid.clone();
    invalid.models[0].revision = 0;
    cases.push(invalid);
    let mut invalid = valid.clone();
    invalid.providers.remove("alpha");
    cases.push(invalid);
    let mut invalid = valid.clone();
    invalid.providers.insert("unused".into(), 1);
    cases.push(invalid);
    let mut invalid = valid.clone();
    invalid.models.push(invalid.models[0].clone());
    invalid
        .models
        .sort_by(|left, right| left.model.cmp(&right.model));
    cases.push(invalid);
    let mut invalid = valid;
    invalid.default_model = model("alpha", "missing");
    cases.push(invalid);
    for invalid in cases {
        assert!(invalid.validate().is_err());
        assert!(invalid.canonical_json_and_digest().is_err());
    }
}
