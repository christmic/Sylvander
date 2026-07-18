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
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
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
#[path = "../tests/unit/tool.rs"]
mod tests;
