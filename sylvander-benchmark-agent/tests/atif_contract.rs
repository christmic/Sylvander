use serde_json::{Map, json};
use sylvander_benchmark_agent::{
    Agent, Metrics, Observation, ObservationResult, Source, Step, ToolCall, Trajectory,
};

fn trajectory() -> Trajectory {
    Trajectory {
        schema_version: "ATIF-v1.7".into(),
        session_id: Some("run-1".into()),
        trajectory_id: Some("trajectory-1".into()),
        agent: Agent {
            name: "sylvander".into(),
            version: "0.1.0".into(),
            model_name: Some("provider/model".into()),
            tool_definitions: None,
        },
        steps: vec![
            Step {
                step_id: 1,
                source: Source::User,
                model_name: None,
                message: "inspect the workspace".into(),
                reasoning_content: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                llm_call_count: None,
            },
            Step {
                step_id: 2,
                source: Source::Agent,
                model_name: Some("provider/model".into()),
                message: String::new(),
                reasoning_content: Some("inspect first".into()),
                tool_calls: Some(vec![ToolCall {
                    tool_call_id: "call-1".into(),
                    function_name: "Command".into(),
                    arguments: Map::from_iter([("command".into(), json!("pwd"))]),
                }]),
                observation: Some(Observation {
                    results: vec![ObservationResult {
                        source_call_id: Some("call-1".into()),
                        content: Some("/workspace".into()),
                        extra: None,
                    }],
                }),
                metrics: Some(Metrics {
                    prompt_tokens: 12,
                    completion_tokens: 4,
                    cached_tokens: Some(2),
                }),
                llm_call_count: Some(1),
            },
        ],
        notes: None,
        final_metrics: None,
        extra: None,
    }
}

#[test]
fn valid_v1_7_trajectory_serializes_without_null_placeholders() {
    let trajectory = trajectory();
    trajectory.validate().unwrap();
    let value = serde_json::to_value(trajectory).unwrap();

    assert_eq!(value["schema_version"], "ATIF-v1.7");
    assert_eq!(value["steps"][1]["llm_call_count"], 1);
    assert!(value["steps"][0].get("metrics").is_none());
}

#[test]
fn unknown_observation_call_reference_fails_closed() {
    let mut trajectory = trajectory();
    trajectory.steps[1].observation.as_mut().unwrap().results[0].source_call_id =
        Some("unknown".into());

    assert_eq!(
        trajectory.validate(),
        Err("observation references an unknown tool call")
    );
}
