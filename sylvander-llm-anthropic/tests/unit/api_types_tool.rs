use super::*;
use serde_json::json;

#[test]
fn tool_basic_wire_format() {
    let tool = Tool::new(
        "get_weather",
        "Get current weather for a location",
        InputSchema::new_with_properties(json!({"location": {"type": "string"}}), &["location"]),
    );
    let json = serde_json::to_string(&tool).unwrap();
    let back: Tool = serde_json::from_str(&json).unwrap();
    assert_eq!(back, tool);
}

#[test]
fn tool_empty_schema_wire_format() {
    let tool = Tool::new("ping", "Health check", InputSchema::empty());
    let json = serde_json::to_string(&tool).unwrap();
    assert_eq!(
        json,
        r#"{"name":"ping","description":"Health check","input_schema":{"type":"object"}}"#
    );
}

#[test]
fn tool_choice_auto() {
    assert_eq!(
        serde_json::to_string(&ToolChoice::auto()).unwrap(),
        r#"{"type":"auto"}"#
    );
}

#[test]
fn tool_choice_any() {
    assert_eq!(
        serde_json::to_string(&ToolChoice::any()).unwrap(),
        r#"{"type":"any"}"#
    );
}

#[test]
fn tool_choice_none() {
    assert_eq!(
        serde_json::to_string(&ToolChoice::none()).unwrap(),
        r#"{"type":"none"}"#
    );
}

#[test]
fn tool_choice_specific_serializes_correctly() {
    let tc = ToolChoice::tool_serial("Read");
    assert_eq!(
        serde_json::to_string(&tc).unwrap(),
        r#"{"type":"tool","name":"Read","disable_parallel_tool_use":true}"#
    );
}

#[test]
fn tool_choice_specific_omits_disable_parallel_when_false() {
    let tc = ToolChoice::tool("Read");
    assert_eq!(
        serde_json::to_string(&tc).unwrap(),
        r#"{"type":"tool","name":"Read"}"#
    );
}

#[test]
fn tool_choice_round_trip() {
    for tc in [
        ToolChoice::auto(),
        ToolChoice::any(),
        ToolChoice::none(),
        ToolChoice::tool("Read"),
        ToolChoice::tool_serial("Bash"),
    ] {
        let s = serde_json::to_string(&tc).unwrap();
        let back: ToolChoice = serde_json::from_str(&s).unwrap();
        assert_eq!(back, tc);
    }
}
