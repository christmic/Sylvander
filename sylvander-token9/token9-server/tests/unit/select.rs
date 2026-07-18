use super::*;

fn target(provider: &str, weight: i64, priority: i64, keys: &[&str]) -> TargetDef {
    TargetDef {
        provider: provider.into(),
        base_url: format!("https://{provider}"),
        dialect: Dialect::Anthropic,
        real_model: "m".into(),
        weight,
        priority,
        keys: keys.iter().map(|s| s.to_string()).collect(),
    }
}

fn rs(targets: Vec<TargetDef>) -> RouteSet {
    RouteSet {
        model_id: "m".into(),
        inject_usage: false,
        targets,
    }
}

#[test]
fn priority_order_is_fallback_sequence() {
    let set = rs(vec![
        target("a", 1, 100, &["k"]),
        target("b", 1, 200, &["k"]),
    ]);
    let lb = LbState::default();
    let attempts = plan(&set, "m", &[], &lb);
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].provider, "a");
    assert_eq!(attempts[0].reason, "primary");
    assert_eq!(attempts[1].provider, "b");
    assert_eq!(attempts[1].reason, "fallback");
}

#[test]
fn rate_limited_provider_skipped() {
    let set = rs(vec![
        target("a", 1, 100, &["k"]),
        target("b", 1, 200, &["k"]),
    ]);
    let rl = vec![RateLimitRow {
        provider: "a".into(),
        updated_at: 0,
        requests_limit: Some(100),
        requests_remaining: Some(0),
        requests_reset: None,
        tokens_limit: None,
        tokens_remaining: None,
        tokens_reset: None,
    }];
    let lb = LbState::default();
    let attempts = plan(&set, "m", &rl, &lb);
    assert_eq!(attempts[0].provider, "b"); // a skipped
}

#[test]
fn weighted_round_robin_spreads_same_tier() {
    let set = rs(vec![
        target("a", 1, 100, &["k"]),
        target("b", 1, 100, &["k"]),
    ]);
    let lb = LbState::default();
    let p0 = plan(&set, "m", &[], &lb)[0].provider.clone();
    let p1 = plan(&set, "m", &[], &lb)[0].provider.clone();
    assert_ne!(p0, p1); // alternates across requests
    assert_eq!(plan(&set, "m", &[], &lb)[0].reason, "load_balance");
}

#[test]
fn key_round_robin() {
    let set = rs(vec![target("a", 1, 100, &["k1", "k2"])]);
    let lb = LbState::default();
    let t0 = plan(&set, "m", &[], &lb)[0].token.clone();
    let t1 = plan(&set, "m", &[], &lb)[0].token.clone();
    assert_ne!(t0, t1);
}
