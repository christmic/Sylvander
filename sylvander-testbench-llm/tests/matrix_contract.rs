use std::collections::BTreeSet;

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
        scenarios: scenarios(&[BenchScenario::Connectivity, BenchScenario::RemoteTokenCount]),
        bindings: vec![
            ProtocolBinding {
                provider_id: "provider-a".into(),
                protocol: "openai_responses".into(),
                endpoint_origin: "https://a.example".into(),
                credential_env: "PROVIDER_A_KEY".into(),
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
                endpoint_origin: "https://b.example".into(),
                credential_env: "PROVIDER_B_KEY".into(),
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
        endpoint_origin: "https://a.example".into(),
        credential_env: "PROVIDER_A_KEY".into(),
        supported_scenarios: scenarios(&[BenchScenario::Connectivity]),
        models: vec![ModelBinding {
            model_id: "model-1".into(),
            advertised_scenarios: scenarios(&[BenchScenario::Connectivity]),
        }],
    };
    let matrix = BenchMatrix {
        schema_version: 1,
        repetitions: 1,
        scenarios: scenarios(&[BenchScenario::Connectivity]),
        bindings: vec![binding.clone(), binding],
    };

    assert_eq!(
        matrix.validate(),
        Err("provider, protocol, and model coordinates must be unique")
    );
}
