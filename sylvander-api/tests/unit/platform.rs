use super::*;
#[test]
fn platform_snapshot_round_trip_keeps_status_semantic() {
    let snapshot = PlatformSnapshot {
        features: vec![PlatformFeature {
            kind: PlatformFeatureKind::Mcp,
            name: "code search".into(),
            status: PlatformFeatureStatus::Configured,
            summary: "configured".into(),
            source: Some("search-mcp".into()),
            trust: Some(PlatformTrust::External),
            auth: PlatformAuthStatus::Configured,
            capabilities: vec!["tools".into()],
            reloadable: false,
        }],
        commands: vec![UiCommandDescriptor {
            id: "review-security".into(),
            name: "security-review".into(),
            usage: "/security-review [scope]".into(),
            description: "Review a selected scope".into(),
            hint: "workspace command".into(),
            source: "agent configuration".into(),
            trust: PlatformTrust::Workspace,
            effect: UiCommandEffect::SubmitPrompt {
                template: "Review {{args}} for security issues.".into(),
            },
        }],
        tool_presentations: vec![ToolPresentationDescriptor {
            tool_name: "search".into(),
            label: "Search".into(),
            kind: ToolPresentationKind::Search,
            target_field: Some("query".into()),
            source: "agent configuration".into(),
            trust: PlatformTrust::Workspace,
        }],
    };
    let json = serde_json::to_string(&snapshot).unwrap();
    let restored: PlatformSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, snapshot);
}
