use super::*;
use crate::test_support::InMemoryToolResultDisk;
use sylvander_llm_anthropic::api::model::ModelInfo;
use sylvander_llm_anthropic::api::types::{
    MessageParam, MessageRole, ToolResultBlock, Usage, UserContent, UserContentBlock,
};

fn model() -> ModelInfo {
    ModelInfo::builder()
        .id("test")
        .context_window(200_000)
        .max_output_tokens(8192)
        .build()
        .unwrap()
}

fn usage() -> Usage {
    Usage {
        input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    }
}

fn user_msg_with_tool_result(tool_use_id: &str, body: &str) -> MessageParam {
    MessageParam {
        role: MessageRole::User,
        content: UserContent::Blocks(vec![UserContentBlock::ToolResult(ToolResultBlock::new(
            tool_use_id,
            body,
        ))]),
    }
}

fn extract_string_body(msg: &MessageParam) -> Option<String> {
    let UserContent::Blocks(blocks) = &msg.content else {
        return None;
    };
    let UserContentBlock::ToolResult(trb) = blocks.first()? else {
        return None;
    };
    match trb.content.as_ref()? {
        ToolResultContent::String(s) => Some(s.clone()),
        ToolResultContent::Blocks(_) => None,
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
    let mut trb = ToolResultBlock::new("toolu_err", &big);
    trb = trb.as_error();
    let mut messages = vec![MessageParam {
        role: MessageRole::User,
        content: UserContent::Blocks(vec![UserContentBlock::ToolResult(trb)]),
    }];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 1);

    // Pull out the block and check its flags.
    let UserContent::Blocks(blocks) = &messages[0].content else {
        panic!("expected blocks");
    };
    let UserContentBlock::ToolResult(trb) = &blocks[0] else {
        panic!("expected tool_result");
    };
    assert_eq!(trb.tool_use_id, "toolu_err");
    assert!(trb.is_error, "is_error must be preserved");
}
