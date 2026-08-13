use sylvander_testbench_llm::{
    BenchResult, BenchScenario, BenchStatus, PassMetrics, RepositoryState, endpoint_origin,
};
use url::Url;

#[test]
fn passed_result_is_versioned_complete_and_content_safe() {
    let result = BenchResult::passed(
        "connectivity_usage",
        1,
        BenchScenario::Connectivity,
        2,
        "provider-a",
        "openai_responses",
        "model-a",
        "https://api.example.test:8443",
        1_800_000_000_000,
        42,
        RepositoryState {
            sylvander_commit: "0123456789abcdef".into(),
            worktree_dirty: false,
        },
        PassMetrics {
            attempts: 1,
            input_tokens: 7,
            output_tokens: 3,
            cache_read_tokens: Some(5),
            ..PassMetrics::default()
        },
    );
    assert_eq!(result.schema_version, 1);
    assert_eq!(result.status, BenchStatus::Passed);
    assert_eq!(result.scenario, BenchScenario::Connectivity);
    assert_eq!(result.run_ordinal, 2);
    let json = serde_json::to_string(&result).unwrap();
    assert!(!json.contains("credential"));
    assert!(!json.contains("authorization"));
    assert!(!json.contains("response_text"));
}

#[test]
fn endpoint_origin_excludes_paths_queries_and_fragments() {
    let url = Url::parse("https://api.example.test:9443/v1/messages?tenant=x#fragment").unwrap();
    assert_eq!(endpoint_origin(&url), "https://api.example.test:9443");
}
