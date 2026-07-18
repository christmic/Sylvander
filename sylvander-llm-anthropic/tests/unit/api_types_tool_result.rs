use super::*;

#[test]
fn string_content_round_trip() {
    let block = ToolResultBlock::new("toolu_abc", "file contents here");
    let json = serde_json::to_string(&block).unwrap();
    let back: ToolResultBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(back, block);
}

#[test]
fn error_marker_serializes() {
    let block = ToolResultBlock::new("toolu_abc", "permission denied").as_error();
    let json = serde_json::to_string(&block).unwrap();
    assert!(json.contains(r#""is_error":true"#));
    let back: ToolResultBlock = serde_json::from_str(&json).unwrap();
    assert!(back.is_error);
}

#[test]
fn cache_control_included_when_set() {
    let block =
        ToolResultBlock::new("toolu_abc", "x").with_cache_control(CacheControl::ephemeral());
    let json = serde_json::to_string(&block).unwrap();
    assert!(json.contains(r#""cache_control":"#));
}

#[test]
fn cache_control_omitted_when_none() {
    let block = ToolResultBlock::new("toolu_abc", "x");
    let json = serde_json::to_string(&block).unwrap();
    assert!(!json.contains("cache_control"));
}

#[test]
fn rich_blocks_serialize_correctly() {
    let blocks = vec![RichToolResultBlock::Text {
        text: "first line".to_string(),
        cache_control: None,
    }];
    let block = ToolResultBlock::with_blocks("toolu_abc", blocks);
    let json = serde_json::to_string(&block).unwrap();
    let back: ToolResultBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(back, block);
}
