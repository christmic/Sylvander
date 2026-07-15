//! Content-free validation for Agent and session prompt inputs.

use std::collections::HashSet;

pub(crate) const MAX_PROMPT_PROFILES: usize = 32;
pub(crate) const MAX_PROMPT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_SESSION_PROMPT_BYTES: usize = 16 * 1024;
pub(crate) const MAX_RESOLVED_PROMPT_BYTES: usize = 128 * 1024;
pub(crate) const MAX_PROMPT_SELECTORS_PER_KIND: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum PromptValidationIssue {
    #[error("too many prompt profiles")]
    TooManyProfiles,
    #[error("prompt content exceeds its size limit")]
    PromptTooLarge,
    #[error("session prompt content exceeds its size limit")]
    SessionPromptTooLarge,
    #[error("resolved prompt content exceeds its size limit")]
    ResolvedPromptTooLarge,
    #[error("prompt content contains a forbidden control character")]
    ForbiddenControlCharacter,
    #[error("session prompt content must not be empty")]
    EmptySessionPrompt,
    #[error("prompt identity must be non-empty and canonical")]
    InvalidIdentity,
    #[error("prompt identities must be unique")]
    DuplicateIdentity,
    #[error("too many prompt selectors")]
    TooManySelectors,
}

pub(crate) fn validate_profile_count(count: usize) -> Result<(), PromptValidationIssue> {
    if count > MAX_PROMPT_PROFILES {
        return Err(PromptValidationIssue::TooManyProfiles);
    }
    Ok(())
}

pub(crate) fn validate_prompt(value: &str) -> Result<(), PromptValidationIssue> {
    validate_content(
        value,
        MAX_PROMPT_BYTES,
        PromptValidationIssue::PromptTooLarge,
    )
}

pub(crate) fn validate_session_prompt(value: &str) -> Result<(), PromptValidationIssue> {
    if value.is_empty() {
        return Err(PromptValidationIssue::EmptySessionPrompt);
    }
    validate_content(
        value,
        MAX_SESSION_PROMPT_BYTES,
        PromptValidationIssue::SessionPromptTooLarge,
    )
}

pub(crate) fn validate_resolved_prompt(value: &str) -> Result<(), PromptValidationIssue> {
    validate_content(
        value,
        MAX_RESOLVED_PROMPT_BYTES,
        PromptValidationIssue::ResolvedPromptTooLarge,
    )
}

pub(crate) fn validate_identity(value: &str) -> Result<(), PromptValidationIssue> {
    if value.is_empty() || value.trim() != value {
        return Err(PromptValidationIssue::InvalidIdentity);
    }
    Ok(())
}

pub(crate) fn validate_unique_identities<'a>(
    values: impl IntoIterator<Item = &'a str>,
    limit: usize,
) -> Result<(), PromptValidationIssue> {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.len() > limit {
        return Err(PromptValidationIssue::TooManySelectors);
    }
    let mut seen = HashSet::with_capacity(values.len());
    for value in values {
        validate_identity(value)?;
        if !seen.insert(value) {
            return Err(PromptValidationIssue::DuplicateIdentity);
        }
    }
    Ok(())
}

fn validate_content(
    value: &str,
    max_bytes: usize,
    too_large: PromptValidationIssue,
) -> Result<(), PromptValidationIssue> {
    if value.len() > max_bytes {
        return Err(too_large);
    }
    if value
        .chars()
        .any(|character| character <= '\u{1f}' && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(PromptValidationIssue::ForbiddenControlCharacter);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
