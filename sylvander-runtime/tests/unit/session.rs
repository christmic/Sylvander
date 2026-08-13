use super::*;
use std::path::PathBuf;
use sylvander_llm_core::{
    ChatMessage, ContentBlock, ModelRef, ModelResponse, StopReason, TokenUsage,
};

fn test_metadata() -> SessionMetadata {
    SessionMetadata {
        workspace: PathBuf::from("/tmp/sylvander-test"),
        name: "test-session".into(),
        user_id: "user-1".into(),
    }
}

#[test]
fn new_session_context_is_empty() {
    let ctx = SessionContext::new(
        SessionId::new("s1"),
        sylvander_api::AgentInstanceId::new("agent-instance-1"),
        test_metadata(),
    );
    assert_eq!(ctx.session_id, SessionId::new("s1"));
    assert!(ctx.is_empty());
    assert_eq!(
        ctx.agent_instance_id,
        sylvander_api::AgentInstanceId::new("agent-instance-1")
    );
    assert_eq!(ctx.len(), 0);
    assert_eq!(ctx.metadata.name, "test-session");
    assert!(ctx.created_at > 0);
    assert_eq!(ctx.created_at, ctx.updated_at);
}

#[test]
fn append_user_message_grows_history() {
    let mut ctx = SessionContext::new(
        SessionId::new("s1"),
        sylvander_api::AgentInstanceId::new("agent-instance-1"),
        test_metadata(),
    );
    ctx.append_user_message(ChatMessage::user("Hello"));
    assert_eq!(ctx.len(), 1);
    assert!(ctx.updated_at >= ctx.created_at);
}

#[test]
fn append_assistant_message_converts_to_param() {
    let mut ctx = SessionContext::new(
        SessionId::new("s1"),
        sylvander_api::AgentInstanceId::new("agent-instance-1"),
        test_metadata(),
    );

    let msg = ModelResponse {
        id: "msg_1".into(),
        content: vec![ContentBlock::Text {
            text: "Hi there!".into(),
        }],
        model: ModelRef::new("test", "test-model"),
        stop_reason: StopReason::EndTurn,
        usage: TokenUsage {
            input_tokens: 5,
            output_tokens: 3,
            ..TokenUsage::default()
        },
    };

    let len_before = ctx.len();
    ctx.append_assistant_message(msg);
    assert_eq!(ctx.len(), len_before + 1);
    assert!(ctx.updated_at >= ctx.created_at);
}

#[test]
fn history_snapshot_is_independent() {
    let mut ctx = SessionContext::new(
        SessionId::new("s1"),
        sylvander_api::AgentInstanceId::new("agent-instance-1"),
        test_metadata(),
    );
    ctx.append_user_message(ChatMessage::user("first"));

    let snap = ctx.history_snapshot();
    assert_eq!(snap.len(), 1);

    // Mutate original — snapshot unchanged
    ctx.append_user_message(ChatMessage::user("second"));
    assert_eq!(snap.len(), 1);
    assert_eq!(ctx.len(), 2);
}

#[test]
fn multiple_sessions_have_independent_histories() {
    let mut ctx_a = SessionContext::new(
        SessionId::new("sa"),
        sylvander_api::AgentInstanceId::new("agent-instance-a"),
        test_metadata(),
    );
    let mut ctx_b = SessionContext::new(
        SessionId::new("sb"),
        sylvander_api::AgentInstanceId::new("agent-instance-b"),
        test_metadata(),
    );

    ctx_a.append_user_message(ChatMessage::user("to A"));
    ctx_b.append_user_message(ChatMessage::user("to B"));

    assert_eq!(ctx_a.len(), 1);
    assert_eq!(ctx_b.len(), 1);
}
