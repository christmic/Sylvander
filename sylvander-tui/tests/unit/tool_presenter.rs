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
fn edit_diff_is_available_inline_and_bounded() {
    let input = serde_json::json!({
        "file_path": "src/main.rs",
        "old_string": "one\ntwo\nthree\nfour\nfive\n",
        "new_string": "one\nTWO\nthree\nFOUR\nfive\n"
    });
    let rows = inline_mutation_rows("edit", &input, 80, 9);
    assert!(rows.len() <= 9);
    assert!(rows.iter().any(|row| row.kind == DetailKind::Removed));
    assert!(rows.iter().any(|row| row.kind == DetailKind::Added));
    assert!(inline_mutation_rows("read", &input, 80, 9).is_empty());
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
fn command_summary_uses_latest_live_line_and_structured_completion() {
    assert_eq!(
        compact_output_summary("Command", None, true, 80),
        "running…"
    );
    assert_eq!(
        compact_output_summary(
            "Command",
            Some("first\n… earlier live output omitted …\nCompiling runtime"),
            true,
            80,
        ),
        "Compiling runtime"
    );
    let completed = serde_json::json!({
        "exit_code": 1,
        "duration_ms": 8421,
        "stdout": "",
        "stderr": "failed",
        "stdout_truncated": false,
        "stderr_truncated": true
    })
    .to_string();
    assert_eq!(
        compact_output_summary("Command", Some(&completed), false, 80),
        "exit 1 · 8421ms · output truncated"
    );
}

#[test]
fn expanded_command_output_keeps_bounded_head_and_tail() {
    let output = (1..=30)
        .map(|number| format!("line {number}"))
        .collect::<Vec<_>>()
        .join("\n");
    let rows = detail_rows(
        "Command",
        &serde_json::json!({"command": "build"}),
        Some(&output),
        false,
        80,
    );
    assert!(rows.iter().any(|row| row.text.contains("line 1")));
    assert!(rows.iter().any(|row| row.text.contains("lines omitted")));
    assert!(rows.iter().any(|row| row.text.contains("line 30")));
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
    assert!(rows.iter().all(|row| !row.text.contains('\u{1b}')));
    assert!(rows.iter().all(|row| !row.text.contains("secret")));
}

#[test]
fn unknown_tool_input_is_generic_bounded_and_redacted() {
    let input = serde_json::json!({
        "operation": "future-mode",
        "api_token": "do-not-render",
        "nested": {"password": "also-hidden"}
    });
    let rows = detail_rows("future_extension_tool", &input, Some("ok"), false, 48);
    assert!(rows.iter().any(|row| row.text.contains("operation")));
    assert!(rows.iter().any(|row| row.text.contains("[REDACTED]")));
    assert!(rows.iter().all(|row| !row.text.contains("do-not-render")));
    assert!(rows.iter().all(|row| !row.text.contains("also-hidden")));
    assert_eq!(
        compact_target("future_extension_tool", &input),
        "future extension tool"
    );
}

#[test]
fn secret_redaction_covers_headers_urls_jwts_and_private_keys() {
    let output = concat!(
        "Authorization: Bearer header-secret\n",
        "AWS_SECRET_ACCESS_KEY=aws-secret\n",
        "postgres://user:db-password@localhost/app\n",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.signature\n",
        "-----BEGIN PRIVATE KEY-----\n",
        "private-material\n",
        "-----END PRIVATE KEY-----\n",
        "src/auth.rs:10:authenticate request\n",
        "safe output"
    );
    let redacted = safe_output(output);
    for secret in [
        "header-secret",
        "aws-secret",
        "db-password",
        "eyJhbGciOiJIUzI1NiJ9",
        "private-material",
    ] {
        assert!(!redacted.contains(secret), "leaked {secret}");
    }
    assert!(redacted.contains("[REDACTED]"));
    assert!(redacted.contains("[REDACTED PRIVATE KEY]"));
    assert!(redacted.contains("src/auth.rs:10:authenticate request"));
    assert!(redacted.contains("safe output"));
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

#[test]
fn declarative_presentations_customize_labels_without_code_callbacks() {
    let presentations = vec![sylvander_protocol::ToolPresentationDescriptor {
        tool_name: "acme_deploy".into(),
        label: "Deploy".into(),
        kind: sylvander_protocol::ToolPresentationKind::Generic,
        target_field: Some("environment".into()),
        source: "agent configuration".into(),
        trust: sylvander_protocol::PlatformTrust::Workspace,
    }];
    assert_eq!(
        compact_target_with_presentation(
            "acme_deploy",
            &serde_json::json!({"environment":"staging"}),
            &presentations,
        ),
        "Deploy staging"
    );
    assert_eq!(
        compact_target_with_presentation("unknown", &serde_json::json!({}), &presentations),
        "unknown"
    );
}
