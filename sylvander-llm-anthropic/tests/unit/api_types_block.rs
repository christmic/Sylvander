use super::*;
use crate::api::types::UserContentBlock;
use serde_json::json;

#[test]
fn text_block_round_trip() {
    let block = ContentBlock::Text(TextBlock::new("Hello"));
    let json = serde_json::to_string(&block).unwrap();
    assert_eq!(json, r#"{"type":"text","text":"Hello"}"#);
    let back: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(back, block);
}

#[test]
fn text_block_with_cache_control() {
    let block =
        ContentBlock::Text(TextBlock::new("Hello").with_cache_control(CacheControl::ephemeral()));
    let json = serde_json::to_string(&block).unwrap();
    assert!(json.contains(r#""cache_control":"#));
}

#[test]
fn thinking_block_round_trip() {
    let block = ContentBlock::Thinking(ThinkingBlock::new("Let me think...", "sig_xxx"));
    let json = serde_json::to_string(&block).unwrap();
    assert_eq!(
        json,
        r#"{"type":"thinking","thinking":"Let me think...","signature":"sig_xxx"}"#
    );
    let back: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(back, block);
}

#[test]
fn tool_use_block_bare_round_trip() {
    let block = ToolUseBlock::new("toolu_abc", "Read", json!({"file_path": "/a/b.txt"}));
    let json = serde_json::to_string(&block).unwrap();
    assert_eq!(
        json,
        r#"{"type":"tool_use","id":"toolu_abc","name":"Read","input":{"file_path":"/a/b.txt"}}"#
    );
    let back: ToolUseBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(back, block);
}

#[test]
fn tool_use_block_via_content_block_round_trip() {
    let cb = ContentBlock::ToolUse(ToolUseBlock::new(
        "toolu_abc",
        "Bash",
        json!({"command": "ls"}),
    ));
    let json = serde_json::to_string(&cb).unwrap();
    assert_eq!(
        json,
        r#"{"type":"tool_use","id":"toolu_abc","name":"Bash","input":{"command":"ls"}}"#
    );
    let back: ContentBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(back, cb);
}

#[test]
fn user_content_string_shorthand() {
    let uc = UserContent::from("hello");
    let json = serde_json::to_string(&uc).unwrap();
    assert_eq!(json, r#""hello""#);
}

#[test]
fn user_content_blocks_round_trip() {
    let uc = UserContent::Blocks(vec![UserContentBlock::text("hi")]);
    let json = serde_json::to_string(&uc).unwrap();
    let back: UserContent = serde_json::from_str(&json).unwrap();
    assert_eq!(back, uc);
}

#[test]
fn content_block_text_helper() {
    let cb = ContentBlock::Text(TextBlock::new("hello"));
    assert_eq!(cb.text(), Some("hello"));
}

#[test]
fn content_block_tool_use_helper() {
    let cb = ContentBlock::ToolUse(ToolUseBlock::new("x", "Bash", json!({})));
    assert!(cb.as_tool_use().is_some());
}
