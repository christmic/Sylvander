use super::*;

fn model(capabilities: impl IntoIterator<Item = &'static str>) -> ModelDefinition {
    ModelDefinition {
        provider_id: "provider".into(),
        model_id: "model".into(),
        revision: 1,
        context_window: 100_000,
        max_output_tokens: 4096,
        capabilities: capabilities.into_iter().map(str::to_owned).collect(),
        lifecycle: ModelLifecycle::Active,
        pricing: None,
    }
}

#[test]
fn parses_every_canonical_capability() {
    let parsed = parse_model_capabilities([
        "extended_thinking",
        "prompt_caching",
        "structured_output",
        "tool_use",
        "vision",
        "document_input",
    ])
    .unwrap();
    assert_eq!(parsed.len(), 6);
    assert!(parsed.contains(&CanonicalModelCapability::ExtendedThinking));
    assert!(parsed.contains(&CanonicalModelCapability::PromptCaching));
    assert!(parsed.contains(&CanonicalModelCapability::StructuredOutput));
    assert!(parsed.contains(&CanonicalModelCapability::ToolUse));
    assert!(parsed.contains(&CanonicalModelCapability::Vision));
    assert!(parsed.contains(&CanonicalModelCapability::DocumentInput));
}

#[test]
fn ingress_alias_is_canonicalized_deterministically() {
    assert_eq!(
        canonicalize_model_capabilities(["reasoning", "tool_use"]).unwrap(),
        BTreeSet::from(["extended_thinking".into(), "tool_use".into()])
    );
    assert!(
        canonicalize_model_capabilities(std::iter::empty::<&str>())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn rejects_unknown_blank_whitespace_and_case_variants() {
    assert_eq!(
        parse_model_capabilities([""]),
        Err(ModelCapabilityError::Blank)
    );
    assert_eq!(
        parse_model_capabilities(["   "]),
        Err(ModelCapabilityError::Blank)
    );
    assert!(matches!(
        parse_model_capabilities([" tool_use"]),
        Err(ModelCapabilityError::SurroundingWhitespace(_))
    ));
    assert!(matches!(
        parse_model_capabilities(["TOOL_USE"]),
        Err(ModelCapabilityError::NotLowercase(_))
    ));
    assert!(matches!(
        parse_model_capabilities(["telepathy"]),
        Err(ModelCapabilityError::Unknown(_))
    ));
}

#[test]
fn rejects_raw_and_alias_semantic_duplicates() {
    assert_eq!(
        parse_model_capabilities(["tool_use", "tool_use"]),
        Err(ModelCapabilityError::Duplicate(
            CanonicalModelCapability::ToolUse
        ))
    );
    assert_eq!(
        parse_model_capabilities(["reasoning", "extended_thinking"]),
        Err(ModelCapabilityError::Duplicate(
            CanonicalModelCapability::ExtendedThinking
        ))
    );
}

#[test]
fn content_free_issue_preserves_each_failure_category() {
    for (capabilities, expected) in [
        (vec![""], ModelCapabilityIssue::Blank),
        (
            vec![" tool_use"],
            ModelCapabilityIssue::SurroundingWhitespace,
        ),
        (vec!["TOOL_USE"], ModelCapabilityIssue::NotLowercase),
        (vec!["future_capability"], ModelCapabilityIssue::Unknown),
        (
            vec!["reasoning", "extended_thinking"],
            ModelCapabilityIssue::Duplicate,
        ),
    ] {
        assert_eq!(
            parse_model_capabilities(capabilities).unwrap_err().issue(),
            expected
        );
    }
}

#[test]
fn model_validation_accepts_historical_alias_without_rewriting_it() {
    let definition = model(["reasoning"]);
    definition.validate().unwrap();
    assert_eq!(
        definition.capabilities,
        BTreeSet::from(["reasoning".into()])
    );
    let (json, _) = canonical_definition(&definition).unwrap();
    assert!(json.contains("reasoning"));
    assert!(!json.contains("extended_thinking"));
}

#[test]
fn model_validation_fails_closed_for_invalid_capability_state() {
    for definition in [
        model(["unknown"]),
        model([" tool_use"]),
        model(["reasoning", "extended_thinking"]),
    ] {
        assert!(matches!(
            definition.validate(),
            Err(AgentRegistryError::Invalid(_))
        ));
    }
}

#[test]
fn validation_error_is_typed_and_does_not_expose_raw_capability() {
    let raw = "secret_future_capability";
    let error = model([raw]).validate().unwrap_err();
    assert!(error.to_string().contains("unknown capability"));
    assert!(!error.to_string().contains(raw));
    assert!(!format!("{error:?}").contains(raw));
    assert_eq!(
        parse_model_capabilities([raw]).unwrap_err().issue(),
        ModelCapabilityIssue::Unknown
    );
}
