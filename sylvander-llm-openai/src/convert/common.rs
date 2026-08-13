//! Shared scalar and media conversion with no protocol state.

use sylvander_llm_core::{MediaSource, ReasoningEffort, ToolResultContent};

use crate::convert::invalid;

pub(super) fn result_text(
    content: &[ToolResultContent],
) -> Result<String, sylvander_llm_core::ProviderError> {
    let mut result = String::new();
    for item in content {
        match item {
            ToolResultContent::Text { text } => result.push_str(text),
            ToolResultContent::Image { .. }
            | ToolResultContent::Audio { .. }
            | ToolResultContent::Document { .. } => {
                return Err(invalid("OpenAI function outputs must be textual"));
            }
        }
    }
    Ok(result)
}

pub(super) fn media_url(source: &MediaSource) -> String {
    match source {
        MediaSource::Url { url } => url.clone(),
        MediaSource::Base64 { media_type, data } => format!("data:{media_type};base64,{data}"),
    }
}

pub(super) const fn effort(value: ReasoningEffort) -> &'static str {
    match value {
        ReasoningEffort::Disabled => "none",
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}
