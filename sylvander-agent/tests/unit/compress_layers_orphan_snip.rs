use super::*;
use sylvander_llm_anthropic::api::model::ModelInfo;
use sylvander_llm_anthropic::api::types::{
    MessageParam, ToolResultBlock, Usage, UserContent, UserContentBlock,
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

fn user_with_tool_result(tool_use_id: &str) -> MessageParam {
    MessageParam {
        role: MessageRole::User,
        content: UserContent::Blocks(vec![UserContentBlock::ToolResult(ToolResultBlock::new(
            tool_use_id,
            "result",
        ))]),
    }
}

fn assistant_with_tool_use(tool_use_id: &str) -> MessageParam {
    MessageParam {
        role: MessageRole::Assistant,
        content: UserContent::Blocks(vec![UserContentBlock::Other(serde_json::json!({
            "type": "tool_use",
            "id": tool_use_id,
            "name": "fake_tool",
            "input": {}
        }))]),
    }
}

#[tokio::test]
async fn removes_tool_result_with_no_matching_tool_use() {
    let layer = OrphanSnipLayer::new();
    let mut messages = vec![user_with_tool_result("orphan_id")];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 1);
    // Block was removed; message is now empty.
    let UserContent::Blocks(blocks) = &messages[0].content else {
        panic!("expected blocks");
    };
    assert!(blocks.is_empty());
}

#[tokio::test]
async fn keeps_tool_result_with_matching_tool_use() {
    let layer = OrphanSnipLayer::new();
    let mut messages = vec![
        assistant_with_tool_use("paired_id"),
        user_with_tool_result("paired_id"),
    ];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    let UserContent::Blocks(blocks) = &messages[1].content else {
        panic!("expected blocks");
    };
    assert_eq!(blocks.len(), 1);
}

#[tokio::test]
async fn removes_multiple_orphans_in_one_pass() {
    let layer = OrphanSnipLayer::new();
    let mut messages = vec![
        user_with_tool_result("orphan_1"),
        user_with_tool_result("paired"),
        assistant_with_tool_use("paired"),
        user_with_tool_result("orphan_2"),
    ];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 2);
    // The paired one remains.
    let UserContent::Blocks(blocks) = &messages[1].content else {
        panic!("expected blocks");
    };
    assert_eq!(blocks.len(), 1);
}

#[tokio::test]
async fn empty_conversation_is_noop() {
    let layer = OrphanSnipLayer::new();
    let mut messages: Vec<MessageParam> = vec![];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    assert_eq!(report.removed_count, 0);
}
