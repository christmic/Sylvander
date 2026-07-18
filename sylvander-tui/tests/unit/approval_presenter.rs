use super::*;

#[test]
fn destructive_shell_is_critical() {
    let summary = summarize("bash", &serde_json::json!({"command": "rm -rf build"}));
    assert_eq!(summary.risk, RiskLevel::Critical);
    assert!(summary.action.starts_with('$'));
}

#[test]
fn read_is_low_risk_and_scoped_to_path() {
    let summary = summarize("read", &serde_json::json!({"path": "src/lib.rs"}));
    assert_eq!(summary.risk, RiskLevel::Low);
    assert_eq!(summary.scope, "src/lib.rs");
}
