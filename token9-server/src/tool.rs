use axum::http::HeaderMap;

/// Logical label for any request that matches no configured rule.
pub const OTHER: &str = "OTHER";

/// A configurable tool-identification rule: if request header `header` contains
/// `pattern` (case-insensitive), the request is attributed to logical `label`.
#[derive(Debug, Clone)]
pub struct ToolRule {
    pub label: String,
    pub header: String,
    pub pattern: String,
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// Logical tool label from config rules (first match wins, by rule order).
/// No match -> "OTHER". Never fabricated.
pub fn logical(headers: &HeaderMap, rules: &[ToolRule]) -> String {
    for r in rules {
        if r.pattern.is_empty() {
            continue;
        }
        let value = header(headers, &r.header).to_lowercase();
        if value.contains(&r.pattern.to_lowercase()) {
            return r.label.clone();
        }
    }
    OTHER.to_string()
}

/// Real tool identifier (raw User-Agent, else originator) — kept so unmapped
/// tools showing up as OTHER can be discovered and given a mapping rule.
pub fn raw(headers: &HeaderMap) -> String {
    let ua = header(headers, "user-agent");
    if !ua.is_empty() {
        return ua.to_string();
    }
    let originator = header(headers, "originator");
    if !originator.is_empty() {
        return originator.to_string();
    }
    OTHER.to_string()
}

#[cfg(test)]
mod tests {
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
            ToolRule { label: "claude-code".into(), header: "user-agent".into(), pattern: "claude-cli".into() },
            ToolRule { label: "codex".into(), header: "originator".into(), pattern: "codex".into() },
        ]
    }

    #[test]
    fn logical_matches_rule() {
        let h = hm(&[("user-agent", "claude-cli/2.1.145 (external, cli)")]);
        assert_eq!(logical(&h, &rules()), "claude-code");
    }

    #[test]
    fn logical_matches_originator() {
        let h = hm(&[("user-agent", "codex_cli_rs/0.5"), ("originator", "codex_cli")]);
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
}
