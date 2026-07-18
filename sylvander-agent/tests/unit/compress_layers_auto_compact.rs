use super::*;
use crate::compress::auto_compact_llm::tests::MockAutoCompactLlm;
use sylvander_llm_anthropic::api::model::ModelInfo;
use sylvander_llm_anthropic::api::types::Usage;

fn model_info() -> ModelInfo {
    ModelInfo::builder()
        .id("test")
        .context_window(1000)
        .max_output_tokens(100)
        .build()
        .unwrap()
}

fn usage_with(input: u32) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: 0,
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    }
}

#[tokio::test]
async fn no_op_when_below_threshold() {
    let layer = AutoCompactLayer::new();
    let mut messages: Vec<MessageParam> = vec![MessageParam::user("hi")];
    let usage = usage_with(100);
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage,
        model_info: &model_info(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.condensed_count, 0);
    assert_eq!(report.removed_count, 0);
}

#[tokio::test]
async fn records_failure_when_llm_not_configured() {
    let layer = AutoCompactLayer::new();
    let mut messages: Vec<MessageParam> = (0..10)
        .map(|i| MessageParam::user(format!("msg {i}")))
        .collect();
    let usage = usage_with(950);
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage,
        model_info: &model_info(),
        auto_compact_llm: None,
    };

    let report = layer.apply(&mut ctx).await;
    assert!(report.failure.is_some());
    assert_eq!(
        report.failure_code,
        Some(CompactionFailureCode::UnsupportedBackend)
    );
    assert_eq!(ctx.messages.len(), 10);
}

#[tokio::test]
async fn summarizes_and_replaces_when_above_threshold() {
    let layer = AutoCompactLayer::new()
        .with_trigger_ratio(0.5)
        .with_keep_last_n_turns(1);
    let mock = MockAutoCompactLlm::new("the concise summary");
    let mut messages: Vec<MessageParam> = (0..6)
        .map(|i| {
            if i % 2 == 0 {
                MessageParam::user(format!("user {i}"))
            } else {
                MessageParam {
                    role: MessageRole::Assistant,
                    content: UserContent::String(format!("asst {i}")),
                }
            }
        })
        .collect();
    let usage = usage_with(600);
    let mut ctx = CompressContext {
        messages: &mut messages,
        last_usage: &usage,
        model_info: &model_info(),
        auto_compact_llm: Some(&mock),
    };

    let report = layer.apply(&mut ctx).await;
    assert_eq!(report.removed_count, 4);
    assert!(report.freed_tokens > 0);

    assert_eq!(ctx.messages.len(), 3);
    let UserContent::String(s) = &ctx.messages[0].content else {
        panic!("expected string");
    };
    assert!(s.contains("the concise summary"));
    // messages[1] = the first kept message = user 4
    if let UserContent::String(s) = &ctx.messages[1].content {
        assert!(s.contains("user 4"));
    } else {
        panic!("expected string");
    }

    assert_eq!(mock.last_messages().len(), 4);
}
