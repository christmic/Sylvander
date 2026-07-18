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

fn user_msg_text(text: &str) -> MessageParam {
    MessageParam {
        role: MessageRole::User,
        content: UserContent::String(text.to_string()),
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

fn first_tool_result_body(msg: &MessageParam) -> Option<String> {
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
async fn keeps_last_n_user_messages_intact() {
    let layer = MicroCompactLayer::new().with_keep_last_n(2);
    let mut messages = vec![
        user_msg_with_tool_result("old_1", "x".repeat(200).as_str()),
        user_msg_with_tool_result("old_2", "y".repeat(200).as_str()),
        user_msg_with_tool_result("recent_1", "z".repeat(200).as_str()),
        user_msg_with_tool_result("recent_2", "w".repeat(200).as_str()),
    ];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 2);
    // The two old ones got placeholders; the two recent ones
    // are intact.
    let body0 = first_tool_result_body(&messages[0]).unwrap();
    assert!(body0.contains("truncated"), "old_1 should be condensed");
    let body2 = first_tool_result_body(&messages[2]).unwrap();
    assert!(
        !body2.contains("truncated"),
        "recent_1 should be intact, got: {body2}"
    );
}

#[tokio::test]
async fn does_not_affect_user_text_messages() {
    let layer = MicroCompactLayer::new().with_keep_last_n(0);
    let mut messages = vec![user_msg_text("user plain text")];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    // User text is unchanged.
    let UserContent::String(s) = &messages[0].content else {
        panic!("expected string");
    };
    assert_eq!(s, "user plain text");
}

#[tokio::test]
async fn zero_keep_condenses_all_tool_results() {
    let layer = MicroCompactLayer::new().with_keep_last_n(0);
    let mut messages = vec![
        user_msg_with_tool_result("a", "x".repeat(100).as_str()),
        user_msg_with_tool_result("b", "y".repeat(100).as_str()),
    ];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 2);
    assert!(report.freed_tokens > 0);
}

#[tokio::test]
async fn empty_conversation_is_noop() {
    let layer = MicroCompactLayer::new();
    let mut messages: Vec<MessageParam> = vec![];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
}

#[tokio::test]
async fn short_tool_results_not_rewritten() {
    // If a tool_result body is already shorter than the
    // placeholder, don't bother rewriting.
    let layer = MicroCompactLayer::new().with_keep_last_n(0);
    let mut messages = vec![user_msg_with_tool_result("a", "short")];
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage(),
        model_info: &model(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
}
