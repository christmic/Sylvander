use super::*;
use serde_json::json;

#[test]
fn serializes_effort_only() {
    let oc = OutputConfig {
        effort: Some(Effort::High),
        format: None,
    };
    assert_eq!(serde_json::to_string(&oc).unwrap(), r#"{"effort":"high"}"#);
}

#[test]
fn serializes_format_only() {
    let schema = json!({"type": "object", "properties": {"answer": {"type": "string"}}});
    let oc = OutputConfig {
        effort: None,
        format: Some(JsonOutputFormat::new(schema.clone())),
    };
    let json = serde_json::to_string(&oc).unwrap();
    let back: OutputConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back.format.unwrap().schema, schema);
}

#[test]
fn serializes_empty_omits_fields() {
    let oc = OutputConfig::default();
    assert_eq!(serde_json::to_string(&oc).unwrap(), r"{}");
}

#[test]
fn json_output_format_wire_format() {
    let schema = json!({"type": "object"});
    let f = JsonOutputFormat::new(schema.clone());
    let json = serde_json::to_string(&f).unwrap();
    let back: JsonOutputFormat = serde_json::from_str(&json).unwrap();
    assert_eq!(back.kind, JsonOutputFormatKind::JsonSchema);
    assert_eq!(back.schema, schema);
}
