use super::*;

#[test]
fn png_block_wire_format() {
    let block = ImageBlock::png("iVBORw0KGgoAAAANSUhEUg==");
    let json = serde_json::to_string(&block).unwrap();
    let back: ImageBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(back, block);
}

#[test]
fn jpeg_block_wire_format() {
    let block = ImageBlock::jpeg("/9j/4AAQS");
    let json = serde_json::to_string(&block).unwrap();
    assert!(json.contains(r#""media_type":"image/jpeg""#));
}

#[test]
fn cache_control_omitted_when_none() {
    let block = ImageBlock::png("xxx");
    let json = serde_json::to_string(&block).unwrap();
    assert!(!json.contains("cache_control"));
}

#[test]
fn cache_control_included_when_set() {
    let block = ImageBlock::png("xxx").with_cache_control(CacheControl::ephemeral());
    let json = serde_json::to_string(&block).unwrap();
    assert!(json.contains(r#""cache_control":"#));
}
