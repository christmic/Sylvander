//! Deterministic pre-dispatch capability validation.
//!
//! The scanner derives required capabilities from both the requested options
//! and prior message content. It reports only the missing capability and
//! triggering feature, never prompt or tool-result content.

use crate::{ContentBlock, ModelCapabilities, ModelRequest, ToolResultContent};
/// Provider-neutral capability required by a request feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequiredModelCapability {
    /// Tool definition or tool-history support.
    ToolUse,
    /// Explicit reasoning request or reasoning-history support.
    Reasoning,
    /// Schema-constrained output support.
    StructuredOutput,
    /// Prompt/tool cache-hint support.
    PromptCaching,
    /// Image input support.
    Vision,
    /// Document input support.
    DocumentInput,
}
impl RequiredModelCapability {
    const fn flag(self) -> ModelCapabilities {
        match self {
            Self::ToolUse => ModelCapabilities::TOOL_USE,
            Self::Reasoning => ModelCapabilities::REASONING,
            Self::StructuredOutput => ModelCapabilities::STRUCTURED_OUTPUT,
            Self::PromptCaching => ModelCapabilities::PROMPT_CACHING,
            Self::Vision => ModelCapabilities::VISION,
            Self::DocumentInput => ModelCapabilities::DOCUMENT_INPUT,
        }
    }
}

impl std::fmt::Display for RequiredModelCapability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ToolUse => "tool_use",
            Self::Reasoning => "reasoning",
            Self::StructuredOutput => "structured_output",
            Self::PromptCaching => "prompt_caching",
            Self::Vision => "vision",
            Self::DocumentInput => "document_input",
        })
    }
}

/// Concrete request feature that triggered a capability requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelRequestFeature {
    /// Non-empty tool definition list.
    ToolDefinitions,
    /// Tool call or result in message history.
    ToolHistory,
    /// Explicit reasoning budget.
    ReasoningRequest,
    /// Reasoning block in message history.
    ReasoningHistory,
    /// Requested output JSON Schema.
    OutputSchema,
    /// Cache hint on a system instruction.
    SystemCacheHint,
    /// Cache hint on a tool definition.
    ToolCacheHint,
    /// Image supplied as a top-level message block.
    DirectImage,
    /// Image supplied inside a tool result.
    ToolResultImage,
    /// Document supplied as a top-level message block.
    DirectDocument,
    /// Document supplied inside a tool result.
    ToolResultDocument,
}

impl std::fmt::Display for ModelRequestFeature {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ToolDefinitions => "tool_definitions",
            Self::ToolHistory => "tool_history",
            Self::ReasoningRequest => "reasoning_request",
            Self::ReasoningHistory => "reasoning_history",
            Self::OutputSchema => "output_schema",
            Self::SystemCacheHint => "system_cache_hint",
            Self::ToolCacheHint => "tool_cache_hint",
            Self::DirectImage => "direct_image",
            Self::ToolResultImage => "tool_result_image",
            Self::DirectDocument => "direct_document",
            Self::ToolResultDocument => "tool_result_document",
        })
    }
}

/// First missing capability and the request feature that requires it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("model lacks `{capability}` required by `{feature}`")]
pub struct ModelRequestCapabilityError {
    /// Missing model capability.
    pub capability: RequiredModelCapability,
    /// Feature that introduced the requirement.
    pub feature: ModelRequestFeature,
}

#[derive(Default)]
struct FeatureScan {
    tool: bool,
    reasoning: bool,
    image: bool,
    result_image: bool,
    document: bool,
    result_document: bool,
}

impl FeatureScan {
    fn visit(&mut self, block: &ContentBlock) {
        match block {
            ContentBlock::ToolCall { .. } => self.tool = true,
            ContentBlock::ToolResult { content, .. } => {
                self.tool = true;
                for item in content {
                    self.result_image |= matches!(item, ToolResultContent::Image { .. });
                    self.result_document |= matches!(item, ToolResultContent::Document { .. });
                }
            }
            ContentBlock::Reasoning { .. } => self.reasoning = true,
            ContentBlock::Image { .. } => self.image = true,
            ContentBlock::Document { .. } => self.document = true,
            ContentBlock::Text { .. } => {}
        }
    }
}

fn requirements(request: &ModelRequest) -> Vec<(RequiredModelCapability, ModelRequestFeature)> {
    let mut scan = FeatureScan::default();
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .for_each(|block| scan.visit(block));
    let tool = (!request.tools.is_empty())
        .then_some(ModelRequestFeature::ToolDefinitions)
        .or(scan.tool.then_some(ModelRequestFeature::ToolHistory));
    let reasoning = request
        .reasoning
        .is_some()
        .then_some(ModelRequestFeature::ReasoningRequest)
        .or(scan
            .reasoning
            .then_some(ModelRequestFeature::ReasoningHistory));
    let cache = request
        .system
        .iter()
        .any(|item| item.cache_hint.is_some())
        .then_some(ModelRequestFeature::SystemCacheHint)
        .or(request
            .tools
            .iter()
            .any(|item| item.cache_hint.is_some())
            .then_some(ModelRequestFeature::ToolCacheHint));
    [
        tool.map(|feature| (RequiredModelCapability::ToolUse, feature)),
        reasoning.map(|feature| (RequiredModelCapability::Reasoning, feature)),
        request.output_schema.is_some().then_some((
            RequiredModelCapability::StructuredOutput,
            ModelRequestFeature::OutputSchema,
        )),
        cache.map(|feature| (RequiredModelCapability::PromptCaching, feature)),
        (scan.image || scan.result_image).then_some((
            RequiredModelCapability::Vision,
            if scan.image {
                ModelRequestFeature::DirectImage
            } else {
                ModelRequestFeature::ToolResultImage
            },
        )),
        (scan.document || scan.result_document).then_some((
            RequiredModelCapability::DocumentInput,
            if scan.document {
                ModelRequestFeature::DirectDocument
            } else {
                ModelRequestFeature::ToolResultDocument
            },
        )),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Derive the union of capabilities required by a normalized request.
pub fn required_model_capabilities(request: &ModelRequest) -> ModelCapabilities {
    requirements(request)
        .into_iter()
        .fold(ModelCapabilities::empty(), |all, (capability, _)| {
            all | capability.flag()
        })
}

/// Reject the first request feature not supported by `available`.
pub fn validate_model_request_capabilities(
    request: &ModelRequest,
    available: ModelCapabilities,
) -> Result<(), ModelRequestCapabilityError> {
    for (capability, feature) in requirements(request) {
        if !available.contains(capability.flag()) {
            return Err(ModelRequestCapabilityError {
                capability,
                feature,
            });
        }
    }
    Ok(())
}
