use crate::{ContentBlock, ModelCapabilities, ModelRequest, ToolResultContent};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequiredModelCapability {
    ToolUse,
    Reasoning,
    StructuredOutput,
    PromptCaching,
    Vision,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelRequestFeature {
    ToolDefinitions,
    ToolHistory,
    ReasoningRequest,
    ReasoningHistory,
    OutputSchema,
    SystemCacheHint,
    ToolCacheHint,
    DirectImage,
    ToolResultImage,
    DirectDocument,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("model lacks `{capability}` required by `{feature}`")]
pub struct ModelRequestCapabilityError {
    pub capability: RequiredModelCapability,
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

pub fn required_model_capabilities(request: &ModelRequest) -> ModelCapabilities {
    requirements(request)
        .into_iter()
        .fold(ModelCapabilities::empty(), |all, (capability, _)| {
            all | capability.flag()
        })
}

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
