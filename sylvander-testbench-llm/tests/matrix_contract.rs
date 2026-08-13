use std::collections::BTreeSet;
use std::fs::File;
use std::path::PathBuf;

use sylvander_testbench_llm::{
    Applicability, BenchMatrix, BenchScenario, ModelBinding, ProtocolBinding,
};

fn scenarios(values: &[BenchScenario]) -> BTreeSet<BenchScenario> {
    values.iter().copied().collect()
}

#[test]
fn expands_protocol_provider_model_scenario_and_run_dimensions() {
    let matrix = BenchMatrix {
        schema_version: 1,
        repetitions: 2,
        request_timeout_ms: 60_000,
        max_output_tokens: 16,
        max_retries: 2,
        scenarios: scenarios(&[BenchScenario::Connectivity, BenchScenario::RemoteTokenCount]),
        bindings: vec![
            ProtocolBinding {
                provider_id: "provider-a".into(),
                protocol: "openai_responses".into(),
                base_url: "https://a.example/v1".into(),
                credential_env: "PROVIDER_A_KEY".into(),
                provider_features: BTreeSet::new(),
                supported_scenarios: scenarios(&[BenchScenario::Connectivity]),
                models: vec![
                    ModelBinding {
                        model_id: "model-1".into(),
                        advertised_scenarios: scenarios(&[BenchScenario::Connectivity]),
                    },
                    ModelBinding {
                        model_id: "model-2".into(),
                        advertised_scenarios: BTreeSet::new(),
                    },
                ],
            },
            ProtocolBinding {
                provider_id: "provider-b".into(),
                protocol: "anthropic_messages".into(),
                base_url: "https://b.example".into(),
                credential_env: "PROVIDER_B_KEY".into(),
                provider_features: BTreeSet::new(),
                supported_scenarios: scenarios(&[
                    BenchScenario::Connectivity,
                    BenchScenario::RemoteTokenCount,
                ]),
                models: vec![ModelBinding {
                    model_id: "model-3".into(),
                    advertised_scenarios: scenarios(&[
                        BenchScenario::Connectivity,
                        BenchScenario::RemoteTokenCount,
                    ]),
                }],
            },
        ],
    };

    let cells = matrix.expand().unwrap();
    assert_eq!(cells.len(), 12);
    assert_eq!(cells[0].coordinate.run_ordinal, 1);
    assert_eq!(cells[1].coordinate.run_ordinal, 2);
    assert!(cells.iter().any(|cell| {
        cell.coordinate.provider_id == "provider-a"
            && cell.coordinate.model_id == "model-1"
            && cell.coordinate.scenario == BenchScenario::RemoteTokenCount
            && cell.applicability == Applicability::NotApplicableProtocol
    }));
    assert!(cells.iter().any(|cell| {
        cell.coordinate.provider_id == "provider-a"
            && cell.coordinate.model_id == "model-2"
            && cell.coordinate.scenario == BenchScenario::Connectivity
            && cell.applicability == Applicability::NotApplicableModel
    }));
    assert!(cells.iter().any(|cell| {
        cell.coordinate.provider_id == "provider-b"
            && cell.coordinate.model_id == "model-3"
            && cell.coordinate.scenario == BenchScenario::RemoteTokenCount
            && cell.applicability == Applicability::Required
    }));
}

#[test]
fn rejects_duplicate_provider_protocol_model_coordinates() {
    let binding = ProtocolBinding {
        provider_id: "provider-a".into(),
        protocol: "openai_chat_completions".into(),
        base_url: "https://a.example/v1".into(),
        credential_env: "PROVIDER_A_KEY".into(),
        provider_features: BTreeSet::new(),
        supported_scenarios: scenarios(&[BenchScenario::Connectivity]),
        models: vec![ModelBinding {
            model_id: "model-1".into(),
            advertised_scenarios: scenarios(&[BenchScenario::Connectivity]),
        }],
    };
    let matrix = BenchMatrix {
        schema_version: 1,
        repetitions: 1,
        request_timeout_ms: 60_000,
        max_output_tokens: 16,
        max_retries: 2,
        scenarios: scenarios(&[BenchScenario::Connectivity]),
        bindings: vec![binding.clone(), binding],
    };

    assert_eq!(
        matrix.validate(),
        Err("provider, protocol, and model coordinates must be unique")
    );
}

#[test]
fn repository_templates_are_valid_multimodel_matrices() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for name in [
        "aliyun-token-plan.live.json",
        "live.example.json",
        "fault.example.json",
        "minimax.live.json",
    ] {
        let matrix: BenchMatrix =
            serde_json::from_reader(File::open(root.join("matrices").join(name)).unwrap()).unwrap();
        let cells = matrix.expand().unwrap();
        assert!(cells.len() > matrix.bindings.len());
        assert!(
            matrix
                .bindings
                .iter()
                .all(|binding| binding.models.len() >= 2)
        );
    }
}
