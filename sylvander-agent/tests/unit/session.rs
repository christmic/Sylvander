use super::*;
use std::path::PathBuf;
use sylvander_llm_anthropic::api::types::MessageParam;

fn test_metadata() -> SessionMetadata {
    SessionMetadata {
        workspace: PathBuf::from("/tmp/sylvander-test"),
        name: "test-session".into(),
        user_id: "user-1".into(),
    }
}

#[test]
fn new_session_context_is_empty() {
    let ctx = SessionContext::new(SessionId::new("s1"), test_metadata());
    assert_eq!(ctx.session_id, SessionId::new("s1"));
    assert!(ctx.is_empty());
    assert_eq!(ctx.len(), 0);
    assert_eq!(ctx.metadata.name, "test-session");
    assert!(ctx.created_at > 0);
    assert_eq!(ctx.created_at, ctx.updated_at);
}

#[test]
fn append_user_message_grows_history() {
    let mut ctx = SessionContext::new(SessionId::new("s1"), test_metadata());
    ctx.append_user_message(MessageParam::user("Hello"));
    assert_eq!(ctx.len(), 1);
    assert!(ctx.updated_at >= ctx.created_at);
}

#[test]
fn append_assistant_message_converts_to_param() {
    let mut ctx = SessionContext::new(SessionId::new("s1"), test_metadata());

    use sylvander_llm_anthropic::api::types::{
        ContentBlock, Message, MessageKind as ApiMessageKind, MessageRole, StopReason, TextBlock,
        Usage, block::TextBlockKind,
    };
    let msg = Message {
        id: "msg_1".into(),
        kind: ApiMessageKind::Message,
        role: MessageRole::Assistant,
        content: vec![ContentBlock::Text(TextBlock {
            kind: TextBlockKind::Text,
            text: "Hi there!".into(),
            cache_control: None,
        })],
        model: "test-model".into(),
        stop_reason: Some(StopReason::EndTurn),
        stop_sequence: None,
        usage: Usage {
            input_tokens: 5,
            output_tokens: 3,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        },
    };

    let len_before = ctx.len();
    ctx.append_assistant_message(msg);
    assert_eq!(ctx.len(), len_before + 1);
    assert!(ctx.updated_at >= ctx.created_at);
}

#[test]
fn history_snapshot_is_independent() {
    let mut ctx = SessionContext::new(SessionId::new("s1"), test_metadata());
    ctx.append_user_message(MessageParam::user("first"));

    let snap = ctx.history_snapshot();
    assert_eq!(snap.len(), 1);

    // Mutate original — snapshot unchanged
    ctx.append_user_message(MessageParam::user("second"));
    assert_eq!(snap.len(), 1);
    assert_eq!(ctx.len(), 2);
}

#[test]
fn multiple_sessions_have_independent_histories() {
    let mut ctx_a = SessionContext::new(SessionId::new("sa"), test_metadata());
    let mut ctx_b = SessionContext::new(SessionId::new("sb"), test_metadata());

    ctx_a.append_user_message(MessageParam::user("to A"));
    ctx_b.append_user_message(MessageParam::user("to B"));

    assert_eq!(ctx_a.len(), 1);
    assert_eq!(ctx_b.len(), 1);
}
