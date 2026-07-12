//! Protocol-neutral formatting for tool activity.
//!
//! The reducer stores exact input/output data. This module turns it into
//! compact, human-readable rows without depending on Ratatui or terminal state.

use serde_json::Value;

const DEFAULT_DETAIL_LIMIT: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailKind {
    Normal,
    Label,
    Added,
    Removed,
    Meta,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailRow {
    pub text: String,
    pub kind: DetailKind,
}

impl DetailRow {
    fn new(text: impl Into<String>, kind: DetailKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}

pub fn detail_rows(
    tool_name: &str,
    input: &Value,
    output: Option<&str>,
    is_error: bool,
    width: usize,
) -> Vec<DetailRow> {
    let mut rows = input_rows(tool_name, input, width);
    if let Some(output) = output.filter(|output| !output.trim().is_empty()) {
        if !rows.is_empty() {
            rows.push(DetailRow::new("", DetailKind::Normal));
        }
        rows.push(DetailRow::new(
            if is_error { "error" } else { "result" },
            if is_error {
                DetailKind::Error
            } else {
                DetailKind::Label
            },
        ));
        if is_shell(tool_name) {
            rows.extend(shell_output_rows(output, width));
        } else if is_search(tool_name) {
            rows.extend(search_output_rows(output, width));
        } else if is_mcp_or_resource(tool_name) {
            rows.extend(resource_output_rows(output, width));
        } else {
            rows.extend(output_rows(output, width, DEFAULT_DETAIL_LIMIT));
        }
    }
    rows
}

pub fn compact_target(tool_name: &str, input: &Value) -> String {
    if let Some((server, tool)) = mcp_identity(tool_name) {
        let target = string_field(input, &["uri", "url", "query", "resource"]);
        return target.map_or_else(
            || format!("MCP {server} · {}", tool.replace('_', " ")),
            |target| {
                format!(
                    "MCP {server} · {} · {}",
                    tool.replace('_', " "),
                    one_line(target)
                )
            },
        );
    }
    match normalized_name(tool_name).as_str() {
        "bash" | "shell" | "exec" => string_field(input, &["command", "cmd"])
            .map(|command| format!("$ {}", one_line(command)))
            .unwrap_or_else(|| tool_name.to_string()),
        "read" | "read_file" => path_with_range(input, "Read"),
        "write" | "write_file" => mutation_target(input, "Write"),
        "edit" | "edit_file" => mutation_target(input, "Edit"),
        "search" | "grep" | "rg" => {
            let query = string_field(input, &["query", "pattern"]).unwrap_or("…");
            let path = string_field(input, &["path", "directory"]).unwrap_or(".");
            format!("Search {query:?} in {path}")
        }
        "ask_user" => string_field(input, &["question"])
            .map(|question| format!("Ask {}", one_line(question)))
            .unwrap_or_else(|| "Ask user".into()),
        "read_memory" | "memory_read" => string_field(input, &["query", "key"])
            .map(|query| format!("Recall {query}"))
            .unwrap_or_else(|| "Read memory".into()),
        "write_memory" | "memory_write" => string_field(input, &["key", "title"])
            .map(|key| format!("Remember {key}"))
            .unwrap_or_else(|| "Write memory".into()),
        "web_search" | "search_web" => string_field(input, &["query"])
            .map(|query| format!("Search web for {query:?}"))
            .unwrap_or_else(|| "Search web".into()),
        "web_fetch" | "fetch_url" | "read_resource" => string_field(input, &["url", "uri"])
            .map(|target| format!("Fetch {target}"))
            .unwrap_or_else(|| tool_name.replace('_', " ")),
        _ => generic_target(tool_name, input),
    }
}

fn input_rows(tool_name: &str, input: &Value, width: usize) -> Vec<DetailRow> {
    let width = width.max(12);
    if let Some((server, tool)) = mcp_identity(tool_name) {
        return resource_input_rows(Some(server), tool, input, width);
    }
    match normalized_name(tool_name).as_str() {
        "bash" | "shell" | "exec" => {
            let mut rows = Vec::new();
            if let Some(cwd) = string_field(input, &["cwd", "workdir"]) {
                rows.push(DetailRow::new(
                    format!("cwd  {}", truncate(cwd, width.saturating_sub(5))),
                    DetailKind::Meta,
                ));
            }
            if let Some(command) = string_field(input, &["command", "cmd"]) {
                rows.extend(
                    wrap_prefixed("$ ", command, width)
                        .into_iter()
                        .map(|row| DetailRow::new(row, DetailKind::Normal)),
                );
            }
            rows
        }
        "read" | "read_file" => file_rows("path", input, width),
        "write" | "write_file" => {
            let files = write_specs(input);
            let mut rows = Vec::new();
            for (index, (path, content)) in files.iter().enumerate() {
                if index > 0 {
                    rows.push(DetailRow::new("", DetailKind::Normal));
                }
                rows.extend(file_heading(path, width));
                rows.extend(unified_diff_rows(path, "", content, width));
            }
            if rows.is_empty() {
                generic_input_rows(input, width)
            } else {
                rows
            }
        }
        "edit" | "edit_file" => {
            let edits = edit_specs(input);
            let mut rows = Vec::new();
            for (index, (path, old, new)) in edits.iter().enumerate() {
                if index > 0 {
                    rows.push(DetailRow::new("", DetailKind::Normal));
                }
                rows.extend(file_heading(path, width));
                rows.extend(unified_diff_rows(path, old, new, width));
            }
            if rows.is_empty() {
                generic_input_rows(input, width)
            } else {
                rows
            }
        }
        "search" | "grep" | "rg" => vec![DetailRow::new(
            compact_target(tool_name, input),
            DetailKind::Normal,
        )],
        "web_search" | "search_web" | "web_fetch" | "fetch_url" | "read_resource"
        | "list_resources" => resource_input_rows(None, tool_name, input, width),
        _ => generic_input_rows(input, width),
    }
}

fn output_rows(output: &str, width: usize, limit: usize) -> Vec<DetailRow> {
    let normalized = safe_output(output);
    let lines = normalized.lines().collect::<Vec<_>>();
    let mut rows = Vec::new();
    for line in lines.iter().take(limit) {
        if line.is_empty() {
            rows.push(DetailRow::new("", DetailKind::Normal));
        } else {
            rows.extend(
                wrap_prefixed("  ", line, width)
                    .into_iter()
                    .map(|row| DetailRow::new(row, DetailKind::Normal)),
            );
        }
    }
    if lines.len() > limit {
        rows.push(DetailRow::new(
            format!("… {} more lines", lines.len() - limit),
            DetailKind::Meta,
        ));
    }
    rows
}

fn unified_diff_rows(path: &str, old: &str, new: &str, width: usize) -> Vec<DetailRow> {
    let before = format!("a/{path}");
    let after = format!("b/{path}");
    let diff = similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&before, &after)
        .to_string();
    diff.lines()
        .map(|line| {
            let kind =
                if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
                    DetailKind::Meta
                } else if line.starts_with('+') {
                    DetailKind::Added
                } else if line.starts_with('-') {
                    DetailKind::Removed
                } else {
                    DetailKind::Normal
                };
            DetailRow::new(truncate(line, width), kind)
        })
        .collect()
}

fn edit_specs(input: &Value) -> Vec<(&str, &str, &str)> {
    let values = input.get("edits").and_then(Value::as_array);
    values.map_or_else(
        || edit_spec(input).into_iter().collect(),
        |values| values.iter().filter_map(edit_spec).collect(),
    )
}

fn edit_spec(value: &Value) -> Option<(&str, &str, &str)> {
    Some((
        string_field(value, &["path", "file_path"])?,
        string_field(value, &["old_string", "old_text", "before"])?,
        string_field(value, &["new_string", "new_text", "after"])?,
    ))
}

fn write_specs(input: &Value) -> Vec<(&str, &str)> {
    let values = input.get("files").and_then(Value::as_array);
    values.map_or_else(
        || write_spec(input).into_iter().collect(),
        |values| values.iter().filter_map(write_spec).collect(),
    )
}

fn write_spec(value: &Value) -> Option<(&str, &str)> {
    Some((
        string_field(value, &["path", "file_path"])?,
        string_field(value, &["content", "new_string", "after"])?,
    ))
}

fn file_heading(path: &str, width: usize) -> Vec<DetailRow> {
    let mut rows = vec![DetailRow::new(
        format!("file  {}", truncate(path, width.saturating_sub(6))),
        DetailKind::Label,
    )];
    if let Some(language) = language_for_path(path) {
        rows.push(DetailRow::new(
            format!("language  {language}"),
            DetailKind::Meta,
        ));
    }
    rows
}

fn shell_output_rows(output: &str, width: usize) -> Vec<DetailRow> {
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return output_rows(output, width, DEFAULT_DETAIL_LIMIT);
    };
    let Some(object) = value.as_object() else {
        return output_rows(output, width, DEFAULT_DETAIL_LIMIT);
    };
    let mut rows = Vec::new();
    let exit = object.get("exit_code").and_then(Value::as_i64);
    let duration = object.get("duration_ms").and_then(Value::as_u64);
    if exit.is_some() || duration.is_some() {
        rows.push(DetailRow::new(
            format!(
                "exit {}{}",
                exit.map_or_else(|| "—".into(), |code| code.to_string()),
                duration.map_or_else(String::new, |ms| format!(" · {ms}ms")),
            ),
            if exit.is_some_and(|code| code != 0) {
                DetailKind::Error
            } else {
                DetailKind::Meta
            },
        ));
    }
    for (label, kind) in [("stdout", DetailKind::Label), ("stderr", DetailKind::Error)] {
        if let Some(content) = object
            .get(label)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            rows.push(DetailRow::new(label, kind));
            rows.extend(output_rows(content, width, DEFAULT_DETAIL_LIMIT));
        }
    }
    rows
}

fn search_output_rows(output: &str, width: usize) -> Vec<DetailRow> {
    let clean = safe_output(output);
    let mut groups = std::collections::BTreeMap::<String, Vec<(String, String)>>::new();
    for line in clean.lines() {
        let mut parts = line.splitn(3, ':');
        let (Some(path), Some(number), Some(text)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if number.parse::<u64>().is_ok() {
            groups
                .entry(path.into())
                .or_default()
                .push((number.into(), text.into()));
        }
    }
    if groups.is_empty() {
        return output_rows(&clean, width, DEFAULT_DETAIL_LIMIT);
    }
    let mut rows = Vec::new();
    for (path, matches) in groups {
        rows.push(DetailRow::new(
            format!(
                "{} · {} matches",
                truncate(&path, width.saturating_sub(14)),
                matches.len()
            ),
            DetailKind::Label,
        ));
        for (number, text) in matches.into_iter().take(6) {
            rows.push(DetailRow::new(
                format!(
                    "  {number:>5}  {}",
                    truncate(text.trim(), width.saturating_sub(10))
                ),
                DetailKind::Normal,
            ));
        }
    }
    rows
}

fn resource_input_rows(
    server: Option<&str>,
    tool: &str,
    input: &Value,
    width: usize,
) -> Vec<DetailRow> {
    let mut rows = Vec::new();
    if let Some(server) = server {
        rows.push(DetailRow::new(
            format!("server  {server}"),
            DetailKind::Label,
        ));
    }
    rows.push(DetailRow::new(
        format!("tool  {}", tool.replace('_', " ")),
        DetailKind::Meta,
    ));
    for (label, names) in [
        ("resource", &["uri", "resource"][..]),
        ("url", &["url"][..]),
        ("query", &["query"][..]),
        ("cursor", &["cursor"][..]),
    ] {
        if let Some(value) = string_field(input, names) {
            rows.extend(
                wrap_prefixed(&format!("{label}  "), value, width)
                    .into_iter()
                    .map(|text| DetailRow::new(text, DetailKind::Normal)),
            );
        }
    }
    let known = ["uri", "resource", "url", "query", "cursor"];
    if input
        .as_object()
        .is_some_and(|map| map.keys().any(|key| !known.contains(&key.as_str())))
    {
        rows.extend(generic_input_rows(input, width));
    }
    rows
}

fn resource_output_rows(output: &str, width: usize) -> Vec<DetailRow> {
    let clean = safe_output(output);
    let Ok(value) = serde_json::from_str::<Value>(&clean) else {
        return output_rows(&clean, width, DEFAULT_DETAIL_LIMIT);
    };
    let mut rows = Vec::new();
    collect_resource_rows(&value, width, &mut rows);
    if rows.is_empty() {
        output_rows(&clean, width, DEFAULT_DETAIL_LIMIT)
    } else {
        rows.truncate(DEFAULT_DETAIL_LIMIT * 2);
        rows
    }
}

fn collect_resource_rows(value: &Value, width: usize, rows: &mut Vec<DetailRow>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_resource_rows(value, width, rows);
            }
        }
        Value::Object(map) => {
            if let Some(uri) = map.get("uri").and_then(Value::as_str) {
                rows.push(DetailRow::new(
                    format!("resource  {}", truncate(uri, width.saturating_sub(10))),
                    DetailKind::Label,
                ));
            }
            if let Some(mime) = map
                .get("mimeType")
                .or_else(|| map.get("mime_type"))
                .and_then(Value::as_str)
            {
                rows.push(DetailRow::new(format!("mime  {mime}"), DetailKind::Meta));
            }
            if let Some(text) = map
                .get("text")
                .or_else(|| map.get("content"))
                .and_then(Value::as_str)
            {
                rows.extend(output_rows(text, width, 8));
            }
            for key in ["contents", "resources", "results"] {
                if let Some(value) = map.get(key) {
                    collect_resource_rows(value, width, rows);
                }
            }
        }
        Value::String(text) => rows.extend(output_rows(text, width, 8)),
        _ => {}
    }
}

fn is_shell(name: &str) -> bool {
    matches!(normalized_name(name).as_str(), "bash" | "shell" | "exec")
}

fn is_search(name: &str) -> bool {
    matches!(normalized_name(name).as_str(), "search" | "grep" | "rg")
}

fn is_mcp_or_resource(name: &str) -> bool {
    mcp_identity(name).is_some()
        || matches!(
            normalized_name(name).as_str(),
            "web_search"
                | "search_web"
                | "web_fetch"
                | "fetch_url"
                | "read_resource"
                | "list_resources"
        )
}

fn mcp_identity(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    rest.split_once("__")
}

fn language_for_path(path: &str) -> Option<&'static str> {
    match std::path::Path::new(path)
        .extension()?
        .to_str()?
        .to_ascii_lowercase()
        .as_str()
    {
        "rs" => Some("Rust"),
        "ts" | "tsx" => Some("TypeScript"),
        "js" | "jsx" => Some("JavaScript"),
        "py" => Some("Python"),
        "go" => Some("Go"),
        "java" => Some("Java"),
        "kt" | "kts" => Some("Kotlin"),
        "md" => Some("Markdown"),
        "json" => Some("JSON"),
        "toml" => Some("TOML"),
        "yaml" | "yml" => Some("YAML"),
        "sh" | "bash" | "zsh" => Some("Shell"),
        _ => None,
    }
}

fn safe_output(output: &str) -> String {
    redact_secrets(&crate::markdown::sanitize_terminal_text(output))
}

fn redact_secrets(output: &str) -> String {
    output
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(|token| {
                    let bare = token.trim_matches(|ch: char| {
                        !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-'
                    });
                    let sensitive_prefix = ["sk-", "ghp_", "github_pat_", "xoxb-", "xoxp-"]
                        .iter()
                        .any(|prefix| bare.starts_with(prefix));
                    let lower = bare.to_ascii_lowercase();
                    let assignment = ["api_key=", "apikey=", "token=", "password="]
                        .iter()
                        .any(|key| lower.contains(key));
                    if sensitive_prefix || assignment {
                        "[REDACTED]"
                    } else {
                        token
                    }
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn file_rows(label: &str, input: &Value, width: usize) -> Vec<DetailRow> {
    let mut rows = Vec::new();
    if let Some(path) = string_field(input, &["path", "file_path"]) {
        rows.push(DetailRow::new(
            format!(
                "{label}  {}",
                truncate(path, width.saturating_sub(label.len() + 2))
            ),
            DetailKind::Normal,
        ));
        if let Some(language) = language_for_path(path) {
            rows.push(DetailRow::new(
                format!("language  {language}"),
                DetailKind::Meta,
            ));
        }
    }
    let offset = number_field(input, &["offset", "line_start"]);
    let limit = number_field(input, &["limit", "line_count"]);
    if offset.is_some() || limit.is_some() {
        rows.push(DetailRow::new(
            format!(
                "range  {}{}",
                offset.map_or_else(|| "start".into(), |value| value.to_string()),
                limit.map_or_else(String::new, |value| format!(" +{value}"))
            ),
            DetailKind::Meta,
        ));
    }
    rows
}

fn generic_input_rows(input: &Value, width: usize) -> Vec<DetailRow> {
    match input {
        Value::Object(map) => map
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| {
                let value = value.as_str().map_or_else(|| value.to_string(), one_line);
                DetailRow::new(
                    format!(
                        "{key}  {}",
                        truncate(&value, width.saturating_sub(key.len() + 2))
                    ),
                    DetailKind::Normal,
                )
            })
            .collect(),
        Value::Null => Vec::new(),
        value => vec![DetailRow::new(
            truncate(&value.to_string(), width),
            DetailKind::Normal,
        )],
    }
}

fn generic_target(tool_name: &str, input: &Value) -> String {
    let candidate = string_field(input, &["path", "query", "command", "key"]);
    candidate.map_or_else(
        || tool_name.replace('_', " "),
        |value| format!("{} {}", tool_name.replace('_', " "), one_line(value)),
    )
}

fn path_with_range(input: &Value, verb: &str) -> String {
    let path = string_field(input, &["path", "file_path"]).unwrap_or("file");
    let offset = number_field(input, &["offset", "line_start"]);
    let limit = number_field(input, &["limit", "line_count"]);
    match (offset, limit) {
        (Some(offset), Some(limit)) => format!("{verb} {path}:{offset}+{limit}"),
        (Some(offset), None) => format!("{verb} {path}:{offset}"),
        _ => format!("{verb} {path}"),
    }
}

fn mutation_target(input: &Value, verb: &str) -> String {
    let count = input
        .get(if verb == "Edit" { "edits" } else { "files" })
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if count > 1 {
        format!("{verb} {count} files")
    } else {
        path_with_range(input, verb)
    }
}

fn normalized_name(name: &str) -> String {
    name.strip_prefix("mcp__")
        .and_then(|name| name.rsplit("__").next())
        .unwrap_or(name)
        .to_ascii_lowercase()
}

fn string_field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
}

fn number_field(value: &Value, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_u64))
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn wrap_prefixed(prefix: &str, value: &str, width: usize) -> Vec<String> {
    let available = width.saturating_sub(prefix.chars().count()).max(1);
    let chars = value.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return vec![prefix.to_string()];
    }
    chars
        .chunks(available)
        .enumerate()
        .map(|(index, chunk)| {
            let prefix = if index == 0 {
                prefix.to_string()
            } else {
                " ".repeat(prefix.chars().count())
            };
            format!("{prefix}{}", chunk.iter().collect::<String>())
        })
        .collect()
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut result = value
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_target_and_details_preserve_command_context() {
        let input = serde_json::json!({"command": "cargo test -p app", "cwd": "/repo"});
        assert_eq!(compact_target("bash", &input), "$ cargo test -p app");
        let rows = detail_rows("bash", &input, Some("ok\n2 passed"), false, 60);
        assert!(rows.iter().any(|row| row.text == "cwd  /repo"));
        assert!(rows.iter().any(|row| row.text == "$ cargo test -p app"));
        assert!(rows.iter().any(|row| row.text == "result"));
    }

    #[test]
    fn edit_details_show_a_compact_diff() {
        let input = serde_json::json!({
            "path": "src/main.rs",
            "old_string": "let old = true;",
            "new_string": "let new = true;"
        });
        let rows = detail_rows("edit", &input, None, false, 60);
        assert!(
            rows.iter()
                .any(|row| row.text.contains("let old = true;") && row.kind == DetailKind::Removed)
        );
        assert!(
            rows.iter()
                .any(|row| row.text.contains("let new = true;") && row.kind == DetailKind::Added)
        );
    }

    #[test]
    fn multi_file_edit_and_write_render_real_file_scoped_diffs() {
        let edit = serde_json::json!({"edits": [
            {"path":"src/a.rs","before":"old a\n","after":"new a\n"},
            {"path":"src/b.rs","before":"old b\n","after":"new b\n"}
        ]});
        assert_eq!(compact_target("edit", &edit), "Edit 2 files");
        let rows = detail_rows("edit", &edit, None, false, 80);
        assert!(rows.iter().any(|row| row.text == "--- a/src/a.rs"));
        assert!(rows.iter().any(|row| row.text == "+++ b/src/b.rs"));

        let write = serde_json::json!({"files": [
            {"path":"new/a.rs","content":"fn a() {}\n"},
            {"path":"new/b.rs","content":"fn b() {}\n"}
        ]});
        assert_eq!(compact_target("write", &write), "Write 2 files");
        let rows = detail_rows("write", &write, None, false, 80);
        assert!(rows.iter().any(|row| row.text == "+++ b/new/a.rs"));
        assert!(
            rows.iter()
                .any(|row| row.text.contains("fn b()") && row.kind == DetailKind::Added)
        );
    }

    #[test]
    fn long_output_is_bounded() {
        let output = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let rows = detail_rows(
            "read",
            &serde_json::json!({"path": "a"}),
            Some(&output),
            false,
            80,
        );
        assert!(rows.iter().any(|row| row.text == "… 8 more lines"));
    }

    #[test]
    fn tool_names_are_case_insensitive() {
        let input = serde_json::json!({"file_path": "Cargo.toml"});
        assert_eq!(compact_target("Read", &input), "Read Cargo.toml");
    }

    #[test]
    fn structured_shell_result_keeps_exit_duration_stdout_and_stderr() {
        let output = serde_json::json!({
            "exit_code": 2,
            "duration_ms": 41,
            "stdout": "one",
            "stderr": "bad"
        })
        .to_string();
        let rows = detail_rows(
            "bash",
            &serde_json::json!({"command":"test"}),
            Some(&output),
            true,
            60,
        );
        assert!(
            rows.iter()
                .any(|row| row.text == "exit 2 · 41ms" && row.kind == DetailKind::Error)
        );
        assert!(
            rows.iter()
                .any(|row| row.text == "stderr" && row.kind == DetailKind::Error)
        );
    }

    #[test]
    fn search_results_group_by_file_and_controls_and_secrets_are_removed() {
        let output =
            "src/a.rs:10:first\nsrc/a.rs:20:\u{1b}[31msecond\u{1b}[0m\nsrc/b.rs:3:token=secret";
        let rows = detail_rows(
            "rg",
            &serde_json::json!({"query":"x"}),
            Some(output),
            false,
            80,
        );
        assert!(rows.iter().any(|row| row.text == "src/a.rs · 2 matches"));
        assert!(rows.iter().all(|row| !row.text.contains("\u{1b}")));
        assert!(rows.iter().all(|row| !row.text.contains("secret")));
    }

    #[test]
    fn mcp_resource_calls_keep_server_resource_and_content_identity() {
        let input = serde_json::json!({"uri":"file:///docs/design.md"});
        assert_eq!(
            compact_target("mcp__filesystem__read_resource", &input),
            "MCP filesystem · read resource · file:///docs/design.md"
        );
        let output = serde_json::json!({"contents":[{
            "uri":"file:///docs/design.md",
            "mimeType":"text/markdown",
            "text":"# Design\nSafe content"
        }]})
        .to_string();
        let rows = detail_rows(
            "mcp__filesystem__read_resource",
            &input,
            Some(&output),
            false,
            80,
        );
        assert!(rows.iter().any(|row| row.text == "server  filesystem"));
        assert!(
            rows.iter()
                .any(|row| row.text == "resource  file:///docs/design.md")
        );
        assert!(rows.iter().any(|row| row.text == "mime  text/markdown"));
        assert!(rows.iter().any(|row| row.text.contains("Safe content")));
    }
}
