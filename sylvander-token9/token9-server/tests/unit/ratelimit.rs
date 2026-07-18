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

#[test]
fn parses_anthropic() {
    let h = hm(&[
        ("anthropic-ratelimit-requests-limit", "1000"),
        ("anthropic-ratelimit-requests-remaining", "999"),
        ("anthropic-ratelimit-requests-reset", "2026-07-08T10:00:00Z"),
        ("anthropic-ratelimit-tokens-limit", "80000"),
        ("anthropic-ratelimit-tokens-remaining", "79000"),
        ("content-type", "application/json"),
    ]);
    let s = parse(Dialect::Anthropic, &h).unwrap();
    assert_eq!(s.requests_limit, Some(1000));
    assert_eq!(s.requests_remaining, Some(999));
    assert_eq!(s.requests_reset.as_deref(), Some("2026-07-08T10:00:00Z"));
    assert_eq!(s.tokens_limit, Some(80000));
    assert_eq!(s.tokens_remaining, Some(79000));
    assert!(s.raw.contains("anthropic-ratelimit-requests-limit"));
}

#[test]
fn parses_openai() {
    let h = hm(&[
        ("x-ratelimit-limit-requests", "500"),
        ("x-ratelimit-remaining-requests", "499"),
        ("x-ratelimit-reset-requests", "6m0s"),
        ("x-ratelimit-remaining-tokens", "120000"),
    ]);
    let s = parse(Dialect::OpenaiChat, &h).unwrap();
    assert_eq!(s.requests_limit, Some(500));
    assert_eq!(s.requests_remaining, Some(499));
    assert_eq!(s.requests_reset.as_deref(), Some("6m0s"));
    assert_eq!(s.tokens_remaining, Some(120000));
}

#[test]
fn none_when_absent() {
    let h = hm(&[("content-type", "application/json")]);
    assert!(parse(Dialect::Anthropic, &h).is_none());
}
