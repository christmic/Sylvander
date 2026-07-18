use super::*;

fn plain(lines: &[Line<'_>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect()
}

#[test]
fn keeps_markdown_structure_and_width_correct_cjk() {
    let lines = render("## 标题\n\n- **重要**：你好世界你好世界", 12);
    let rows = plain(&lines);
    assert_eq!(rows[0], "标题");
    assert!(rows.iter().any(|row| row.starts_with("• 重要")));
    assert!(
        rows.iter()
            .all(|row| UnicodeWidthStr::width(row.as_str()) <= 12)
    );
}

#[test]
fn renders_code_links_quotes_and_narrow_table_semantically() {
    let source = "> See [docs](https://example.com)\n\n```rs\nlet x = 1;\n```\n\n| A | B |\n|---|---|\n| x | y |";
    let rows = plain(&render(source, 30));
    assert!(rows.iter().any(|row| row.starts_with("│ See docs")));
    assert!(rows.iter().any(|row| row == "│ let x = 1;"));
    assert!(rows.iter().any(|row| row == "A: x"));
}

#[test]
fn strips_terminal_control_sequences_without_losing_text() {
    assert_eq!(
        sanitize_terminal_text("ok\u{1b}[31mRED\u{1b}[0m\u{7}"),
        "okRED"
    );
}

#[test]
fn incomplete_inline_markers_keep_streaming_geometry_stable() {
    let partial = plain(&render("Status: **working", 12));
    let settled = plain(&render("Status: **working**", 12));
    assert_eq!(partial, settled);
}

#[test]
fn wrapping_never_splits_emoji_or_combining_graphemes() {
    let rows = plain(&render(
        "👩‍💻 e\u{301}cole https://example.com/very/long/path",
        12,
    ));
    assert!(
        rows.iter()
            .all(|row| UnicodeWidthStr::width(row.as_str()) <= 12)
    );
    assert!(rows.iter().any(|row| row.contains("👩‍💻")));
    assert!(rows.iter().any(|row| row.contains("e\u{301}")));
    assert!(!rows.iter().any(|row| row.starts_with('\u{301}')));
}
