use std::collections::BTreeSet;

use sylvander_testbench_agent::matrix::{
    AgentBenchMatrix, AgentCapability, Applicability, BenchmarkBinding, DeploymentBinding,
    TaskBinding,
};

#[test]
fn expands_every_external_task_deployment_and_run_coordinate() {
    let matrix = AgentBenchMatrix {
        schema_version: 1,
        repetitions: 2,
        benchmarks: vec![BenchmarkBinding {
            benchmark_id: "harbor".into(),
            dataset_name: "terminal-bench".into(),
            dataset_version: "2.0".into(),
            tasks: vec![TaskBinding {
                task_id: "task-a".into(),
                required_capabilities: BTreeSet::from([AgentCapability::Terminal]),
            }],
        }],
        deployments: vec![
            DeploymentBinding {
                agent_revision: "agent-r1".into(),
                provider_id: "minimax".into(),
                protocol: "openai_chat_completions".into(),
                model_id: "MiniMax-M2.7".into(),
                capabilities: BTreeSet::from([AgentCapability::Terminal]),
            },
            DeploymentBinding {
                agent_revision: "agent-r1".into(),
                provider_id: "provider-b".into(),
                protocol: "anthropic_messages".into(),
                model_id: "model-b".into(),
                capabilities: BTreeSet::new(),
            },
        ],
    };

    let cells = matrix.expand().unwrap();
    assert_eq!(cells.len(), 4);
    assert_eq!(cells[0].applicability, Applicability::Required);
    assert_eq!(
        cells[2].applicability,
        Applicability::NotApplicableCapability
    );
    assert_eq!(cells[1].coordinate.run_ordinal, 2);
    assert_eq!(cells[0].coordinate.dataset_version, "2.0");
}

#[test]
fn rejects_an_unversioned_dataset() {
    let matrix = AgentBenchMatrix {
        schema_version: 1,
        repetitions: 1,
        benchmarks: vec![BenchmarkBinding {
            benchmark_id: "harbor".into(),
            dataset_name: "terminal-bench".into(),
            dataset_version: String::new(),
            tasks: vec![TaskBinding {
                task_id: "task-a".into(),
                required_capabilities: BTreeSet::new(),
            }],
        }],
        deployments: vec![DeploymentBinding {
            agent_revision: "r1".into(),
            provider_id: "provider".into(),
            protocol: "protocol".into(),
            model_id: "model".into(),
            capabilities: BTreeSet::new(),
        }],
    };

    assert!(matrix.expand().is_err());
}
