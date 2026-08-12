use super::*;
use crate::test_support::InMemoryToolResultDisk;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ModelCapabilities, ModelInfo, ModelRef, TokenUsage,
    ToolResultContent,
};

fn model() -> ModelInfo {
    ModelInfo {
        reference: ModelRef::new("test", "test"),
        context_window: 200_000,
        max_output_tokens: 8192,
        capabilities: ModelCapabilities::empty(),
    }
}

fn usage() -> TokenUsage {
    TokenUsage::default()
}

fn user_msg_with_tool_result(tool_use_id: &str, body: &str) -> ChatMessage {
    ChatMessage::user_blocks(vec![ContentBlock::tool_result_text(
        tool_use_id,
        body,
        false,
    )])
}

fn extract_string_body(msg: &ChatMessage) -> Option<String> {
    let ContentBlock::ToolResult { content, .. } = msg.content.first()? else {
        return None;
    };
    match content.first()? {
        ToolResultContent::Text { text } => Some(text.clone()),
        _ => None,
    }
}

#[tokio::test]
async fn no_op_when_all_under_budget() {
    let disk = Arc::new(InMemoryToolResultDisk::new());
    let layer = ToolResultBudgetLayer::new(disk.clone());

    let mut messages = vec![
        user_msg_with_tool_result("a", "short"),
        user_msg_with_tool_result("b", "also short"),
    ];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    assert_eq!(disk.write_count(), 0);
}

#[tokio::test]
async fn writes_to_disk_and_replaces_with_preview() {
    let disk = Arc::new(InMemoryToolResultDisk::new());
    let layer = ToolResultBudgetLayer::new(disk.clone())
        .with_max_inline_chars(50)
        .with_preview_chars(20);

    let big = "x".repeat(200);
    let mut messages = vec![user_msg_with_tool_result("toolu_big", &big)];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 1);
    assert_eq!(report.removed_count, 0);
    assert!(report.freed_tokens > 0);
    assert_eq!(disk.write_count(), 1);
    assert_eq!(disk.get("toolu_big").as_deref(), Some(big.as_str()));

    let rewritten = extract_string_body(&messages[0]).unwrap();
    assert!(rewritten.starts_with("[Output saved to "));
    assert!(rewritten.contains("first 20 chars shown"));
    // The original 200 x's were reduced; preview should be <= 20 chars.
    assert!(rewritten.len() < 200);
}

#[tokio::test]
async fn mixed_sizes_only_rewrites_oversized() {
    let disk = Arc::new(InMemoryToolResultDisk::new());
    let layer = ToolResultBudgetLayer::new(disk.clone())
        .with_max_inline_chars(100)
        .with_preview_chars(30);

    let big = "B".repeat(200);
    let mut messages = vec![
        user_msg_with_tool_result("small", "tiny"),
        user_msg_with_tool_result("big", &big),
        user_msg_with_tool_result("medium", "medium-sized body here, well under limit"),
    ];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 1);
    assert_eq!(disk.write_count(), 1);
    assert_eq!(disk.ids(), vec!["big".to_string()]);
}

#[tokio::test]
async fn preserves_is_error_and_tool_use_id() {
    // We don't directly test the disk-error path here (would need
    // a fault-injecting disk) — but we verify that the rewrite
    // keeps the tool_use_id and is_error flags intact.
    let disk = Arc::new(InMemoryToolResultDisk::new());
    let layer = ToolResultBudgetLayer::new(disk.clone())
        .with_max_inline_chars(50)
        .with_preview_chars(20);

    let big = "y".repeat(200);
    let mut messages = vec![ChatMessage::user_blocks(vec![
        ContentBlock::tool_result_text("toolu_err", &big, true),
    ])];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 1);

    // Pull out the block and check its flags.
    let ContentBlock::ToolResult {
        call_id, is_error, ..
    } = &messages[0].content[0]
    else {
        panic!("expected tool_result");
    };
    assert_eq!(call_id, "toolu_err");
    assert!(*is_error, "is_error must be preserved");
}
