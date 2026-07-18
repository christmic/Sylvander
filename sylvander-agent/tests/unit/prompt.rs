use super::*;

fn selection(provider: &str, model: &str) -> ModelSelection {
    ModelSelection {
        provider_id: provider.into(),
        model_id: model.into(),
    }
}

#[test]
fn content_limits_are_byte_based_and_content_free() {
    assert_eq!(
        validate_prompt(&"界".repeat(MAX_PROMPT_BYTES / 3 + 1)),
        Err(PromptValidationIssue::PromptTooLarge)
    );
    assert_eq!(
        validate_session_prompt(""),
        Err(PromptValidationIssue::EmptySessionPrompt)
    );
    let secret = "secret\0prompt";
    let error = validate_prompt(secret).unwrap_err();
    assert_eq!(error, PromptValidationIssue::ForbiddenControlCharacter);
    assert!(!error.to_string().contains(secret));
    validate_prompt("line one\nline two\r\n\tindented").unwrap();
}

#[test]
fn identities_are_exact_and_unique() {
    for invalid in ["", " model", "model ", "\tmodel"] {
        assert_eq!(
            validate_identity(invalid),
            Err(PromptValidationIssue::InvalidIdentity)
        );
    }
    assert_eq!(
        validate_unique_identities(["model", "model"], 64),
        Err(PromptValidationIssue::DuplicateIdentity)
    );
    assert_eq!(
        validate_unique_identities(std::iter::repeat_n("model", 65), 64),
        Err(PromptValidationIssue::TooManySelectors)
    );
}

#[test]
fn resolver_composes_non_overridable_layers_and_exact_manifest() {
    let prompt_policy = PromptResolver::new(
        "agent:sylvander@7".into(),
        "agent instructions".into(),
        vec![PromptProfile {
            id: "alpha-coding".into(),
            qualified_models: vec![selection("alpha", "shared")],
            providers: Vec::new(),
            models: Vec::new(),
            system_prompt: "profile instructions".into(),
        }],
        Some("alpha-coding".into()),
        true,
    )
    .unwrap();

    let composed = prompt_policy
        .resolve(
            &selection("alpha", "shared"),
            None,
            Some("session instructions"),
        )
        .unwrap();
    assert_eq!(
        composed.system_prompt,
        format!(
            "{SHARED_SAFETY_PROMPT}\n\nprofile instructions\n\nagent instructions\n\nsession instructions"
        )
    );
    assert_eq!(
        composed
            .manifest
            .layers
            .iter()
            .map(|layer| layer.kind)
            .collect::<Vec<_>>(),
        vec![
            PromptLayerKind::SharedSafety,
            PromptLayerKind::ProviderModelProfile,
            PromptLayerKind::Agent,
            PromptLayerKind::SessionInput,
        ]
    );
    assert_eq!(
        composed.manifest.total_bytes,
        composed
            .manifest
            .layers
            .iter()
            .map(|layer| layer.byte_count)
            .sum::<u64>()
    );
    assert_ne!(
        composed.manifest.aggregate_sha256,
        digest(&composed.system_prompt)
    );
}

#[test]
fn resolver_rejects_incompatible_disabled_and_oversized_compositions() {
    let resolver = PromptResolver::new(
        "agent:sylvander@1".into(),
        "a".repeat(MAX_PROMPT_BYTES),
        vec![PromptProfile {
            id: "alpha".into(),
            qualified_models: vec![selection("alpha", "model-a")],
            providers: Vec::new(),
            models: Vec::new(),
            system_prompt: "p".repeat(MAX_PROMPT_BYTES),
        }],
        Some("alpha".into()),
        false,
    )
    .unwrap();
    assert_eq!(
        resolver.resolve(&selection("beta", "model-a"), None, None),
        Err(PromptResolveError::IncompatibleProfile)
    );
    assert_eq!(
        resolver.resolve(&selection("alpha", "model-a"), None, Some("session")),
        Err(PromptResolveError::SessionPromptDisabled)
    );
    assert_eq!(
        resolver.resolve(&selection("alpha", "model-a"), None, None),
        Err(PromptResolveError::Invalid)
    );
}

#[test]
fn qualified_profiles_do_not_cross_same_named_models() {
    let prompt_policy = PromptResolver::new(
        "agent:sylvander@2".into(),
        "agent".into(),
        vec![
            PromptProfile {
                id: "alpha".into(),
                qualified_models: vec![selection("alpha", "shared")],
                providers: Vec::new(),
                models: Vec::new(),
                system_prompt: "alpha profile".into(),
            },
            PromptProfile {
                id: "beta".into(),
                qualified_models: vec![selection("beta", "shared")],
                providers: Vec::new(),
                models: Vec::new(),
                system_prompt: "beta profile".into(),
            },
        ],
        None,
        false,
    )
    .unwrap();
    let beta = prompt_policy
        .resolve(&selection("beta", "shared"), Some("beta"), None)
        .unwrap();
    assert!(beta.system_prompt.contains("beta profile"));
    assert!(!beta.system_prompt.contains("alpha profile"));
    assert_eq!(
        prompt_policy.resolve(&selection("beta", "shared"), Some("alpha"), None),
        Err(PromptResolveError::IncompatibleProfile)
    );
    assert!(
        validate_profile_selectors(&[], &["alpha".into(), "beta".into()], &["shared".into()])
            .is_err()
    );
}
