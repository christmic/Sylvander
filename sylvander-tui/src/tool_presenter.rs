//! Protocol-neutral formatting for tool activity.
//!
//! The reducer stores exact input/output data. This module turns it into
//! compact, human-readable rows without depending on Ratatui or terminal state.

use serde_json::Value;

const DEFAULT_DETAIL_LIMIT: usize = 12;

pub fn detail_rows(
    tool_name: &str,
    input: &Value,
    output: Option<&str>,
    is_error: bool,
    width: usize,
) -> Vec<String> {
    let mut rows = input_rows(tool_name, input, width);
    if let Some(output) = output.filter(|output| !output.trim().is_empty()) {
        if !rows.is_empty() {
            rows.push(String::new());
        }
        rows.push(if is_error {
            "error".into()
        } else {
            "result".into()
        });
        rows.extend(output_rows(output, width, DEFAULT_DETAIL_LIMIT));
    }
    rows
}

pub fn compact_target(tool_name: &str, input: &Value) -> String {
    match normalized_name(tool_name).as_str() {
        "bash" | "shell" | "exec" => string_field(input, &["command", "cmd"])
            .map(|command| format!("$ {}", one_line(command)))
            .unwrap_or_else(|| tool_name.to_string()),
        "read" | "read_file" => path_with_range(input, "Read"),
        "write" | "write_file" => path_with_range(input, "Write"),
        "edit" | "edit_file" => path_with_range(input, "Edit"),
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
        _ => generic_target(tool_name, input),
    }
}

fn input_rows(tool_name: &str, input: &Value, width: usize) -> Vec<String> {
    let width = width.max(12);
    match normalized_name(tool_name).as_str() {
        "bash" | "shell" | "exec" => {
            let mut rows = Vec::new();
            if let Some(cwd) = string_field(input, &["cwd", "workdir"]) {
                rows.push(format!("cwd  {}", truncate(cwd, width.saturating_sub(5))));
            }
            if let Some(command) = string_field(input, &["command", "cmd"]) {
                rows.extend(wrap_prefixed("$ ", command, width));
            }
            rows
        }
        "read" | "read_file" => file_rows("path", input, width),
        "write" | "write_file" => {
            let mut rows = file_rows("path", input, width);
            if let Some(content) = string_field(input, &["content"]) {
                rows.push(format!("content  {} lines", content.lines().count()));
            }
            rows
        }
        "edit" | "edit_file" => {
            let mut rows = file_rows("path", input, width);
            if let Some(old) = string_field(input, &["old_string", "old_text"]) {
                rows.push(format!(
                    "- {}",
                    truncate(&one_line(old), width.saturating_sub(2))
                ));
            }
            if let Some(new) = string_field(input, &["new_string", "new_text"]) {
                rows.push(format!(
                    "+ {}",
                    truncate(&one_line(new), width.saturating_sub(2))
                ));
            }
            rows
        }
        "search" | "grep" | "rg" => vec![compact_target(tool_name, input)],
        _ => generic_input_rows(input, width),
    }
}

fn output_rows(output: &str, width: usize, limit: usize) -> Vec<String> {
    let normalized = output.replace('\r', "");
    let lines = normalized.lines().collect::<Vec<_>>();
    let mut rows = Vec::new();
    for line in lines.iter().take(limit) {
        if line.is_empty() {
            rows.push(String::new());
        } else {
            rows.extend(wrap_prefixed("  ", line, width));
        }
    }
    if lines.len() > limit {
        rows.push(format!("… {} more lines", lines.len() - limit));
    }
    rows
}

fn file_rows(label: &str, input: &Value, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    if let Some(path) = string_field(input, &["path", "file_path"]) {
        rows.push(format!(
            "{label}  {}",
            truncate(path, width.saturating_sub(label.len() + 2))
        ));
    }
    let offset = number_field(input, &["offset", "line_start"]);
    let limit = number_field(input, &["limit", "line_count"]);
    if offset.is_some() || limit.is_some() {
        rows.push(format!(
            "range  {}{}",
            offset.map_or_else(|| "start".into(), |value| value.to_string()),
            limit.map_or_else(String::new, |value| format!(" +{value}"))
        ));
    }
    rows
}

fn generic_input_rows(input: &Value, width: usize) -> Vec<String> {
    match input {
        Value::Object(map) => map
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| {
                let value = value.as_str().map_or_else(|| value.to_string(), one_line);
                format!(
                    "{key}  {}",
                    truncate(&value, width.saturating_sub(key.len() + 2))
                )
            })
            .collect(),
        Value::Null => Vec::new(),
        value => vec![truncate(&value.to_string(), width)],
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
        assert!(rows.iter().any(|row| row == "cwd  /repo"));
        assert!(rows.iter().any(|row| row == "$ cargo test -p app"));
        assert!(rows.iter().any(|row| row == "result"));
    }

    #[test]
    fn edit_details_show_a_compact_diff() {
        let input = serde_json::json!({
            "path": "src/main.rs",
            "old_string": "let old = true;",
            "new_string": "let new = true;"
        });
        let rows = detail_rows("edit", &input, None, false, 60);
        assert!(rows.iter().any(|row| row == "- let old = true;"));
        assert!(rows.iter().any(|row| row == "+ let new = true;"));
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
        assert!(rows.iter().any(|row| row == "… 8 more lines"));
    }

    #[test]
    fn tool_names_are_case_insensitive() {
        let input = serde_json::json!({"file_path": "Cargo.toml"});
        assert_eq!(compact_target("Read", &input), "Read Cargo.toml");
    }
}
