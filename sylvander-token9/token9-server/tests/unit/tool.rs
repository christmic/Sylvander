use super::*;

fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.insert(
            axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            v.parse().unwrap(),
        );
    }
    h
}

fn rules() -> Vec<ToolRule> {
    vec![
        ToolRule {
            label: "claude-code".into(),
            header: "user-agent".into(),
            pattern: "claude-cli".into(),
        },
        ToolRule {
            label: "codex".into(),
            header: "originator".into(),
            pattern: "codex".into(),
        },
    ]
}

#[test]
fn logical_matches_rule() {
    let h = hm(&[("user-agent", "claude-cli/2.1.145 (external, cli)")]);
    assert_eq!(logical(&h, &rules()), "claude-code");
}

#[test]
fn logical_matches_originator() {
    let h = hm(&[
        ("user-agent", "codex_cli_rs/0.5"),
        ("originator", "codex_cli"),
    ]);
    assert_eq!(logical(&h, &rules()), "codex");
}

#[test]
fn unmatched_is_other() {
    let h = hm(&[("user-agent", "qoder-cli/1.0")]);
    assert_eq!(logical(&h, &rules()), "OTHER");
}

#[test]
fn raw_keeps_user_agent() {
    let h = hm(&[("user-agent", "qoder-cli/1.0 (macos)")]);
    assert_eq!(raw(&h), "qoder-cli/1.0 (macos)");
}

#[test]
fn raw_other_when_absent() {
    let h = hm(&[("content-type", "application/json")]);
    assert_eq!(raw(&h), "OTHER");
}
