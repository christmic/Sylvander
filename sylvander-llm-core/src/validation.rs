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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        CacheHint, ChatMessage, ChatRole, DocumentContent, ImageContent, MediaSource, ModelRef,
        ReasoningConfig, SystemInstruction, ToolDefinition,
    };

    fn request() -> ModelRequest {
        ModelRequest {
            request_id: "secret-request".into(),
            model: ModelRef::new("provider", "model"),
            system: vec![],
            messages: vec![ChatMessage::user("secret-message")],
            tools: vec![],
            max_output_tokens: 100,
            reasoning: None,
            output_schema: None,
        }
    }

    fn image() -> ImageContent {
        ImageContent {
            source: MediaSource::Url {
                url: "https://secret.invalid/image".into(),
            },
            alt_text: None,
        }
    }

    fn document() -> DocumentContent {
        DocumentContent {
            source: MediaSource::Url {
                url: "https://secret.invalid/document".into(),
            },
            title: None,
        }
    }

    fn tool(cache_hint: Option<CacheHint>) -> ToolDefinition {
        ToolDefinition {
            name: "secret-tool".into(),
            description: "secret-description".into(),
            input_schema: json!({"secret-schema": true}),
            cache_hint,
        }
    }

    macro_rules! missing {
        ($input:expr, $available:expr, $capability:expr, $feature:expr) => {
            assert_eq!(
                validate_model_request_capabilities($input, $available),
                Err(ModelRequestCapabilityError {
                    capability: $capability,
                    feature: $feature,
                })
            );
        };
    }

    #[test]
    fn request_features_require_all_six_capabilities_and_errors_are_redacted() {
        let mut input = request();
        input.tools.push(tool(None));
        input.reasoning = Some(ReasoningConfig { budget_tokens: 10 });
        input.output_schema = Some(json!({"secret-output-schema": true}));
        input.system.push(SystemInstruction {
            text: "secret-system".into(),
            cache_hint: Some(CacheHint::Ephemeral),
        });
        input.messages.push(ChatMessage {
            role: ChatRole::User,
            content: vec![
                ContentBlock::Image { image: image() },
                ContentBlock::Document {
                    document: document(),
                },
            ],
        });
        let tool = ModelCapabilities::TOOL_USE;
        let reasoning = tool | ModelCapabilities::REASONING;
        let schema = reasoning | ModelCapabilities::STRUCTURED_OUTPUT;
        let cache = schema | ModelCapabilities::PROMPT_CACHING;
        let vision = cache | ModelCapabilities::VISION;
        let all = vision | ModelCapabilities::DOCUMENT_INPUT;
        assert_eq!(required_model_capabilities(&input), all);
        for (available, capability, feature) in [
            (
                ModelCapabilities::empty(),
                RequiredModelCapability::ToolUse,
                ModelRequestFeature::ToolDefinitions,
            ),
            (
                tool,
                RequiredModelCapability::Reasoning,
                ModelRequestFeature::ReasoningRequest,
            ),
            (
                reasoning,
                RequiredModelCapability::StructuredOutput,
                ModelRequestFeature::OutputSchema,
            ),
            (
                schema,
                RequiredModelCapability::PromptCaching,
                ModelRequestFeature::SystemCacheHint,
            ),
            (
                cache,
                RequiredModelCapability::Vision,
                ModelRequestFeature::DirectImage,
            ),
            (
                vision,
                RequiredModelCapability::DocumentInput,
                ModelRequestFeature::DirectDocument,
            ),
        ] {
            missing!(&input, available, capability, feature);
        }
        validate_model_request_capabilities(&input, all).unwrap();
        let rendered = format!(
            "{:?}",
            validate_model_request_capabilities(&input, ModelCapabilities::empty()).unwrap_err()
        );
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn history_and_nested_media_stack_requirements() {
        let mut input = request();
        input.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: vec![ContentBlock::ToolCall {
                id: "secret-call".into(),
                name: "secret-tool".into(),
                arguments: json!({}),
            }],
        });
        missing!(
            &input,
            ModelCapabilities::empty(),
            RequiredModelCapability::ToolUse,
            ModelRequestFeature::ToolHistory
        );
        let mut input = request();
        input.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: vec![ContentBlock::Reasoning {
                text: "secret-reasoning".into(),
                opaque_state: None,
            }],
        });
        input.messages.push(ChatMessage {
            role: ChatRole::User,
            content: vec![ContentBlock::ToolResult {
                call_id: "secret-call".into(),
                content: vec![
                    ToolResultContent::Image { image: image() },
                    ToolResultContent::Document {
                        document: document(),
                    },
                ],
                is_error: false,
            }],
        });
        let tool = ModelCapabilities::TOOL_USE;
        let reasoning = tool | ModelCapabilities::REASONING;
        let vision = reasoning | ModelCapabilities::VISION;
        assert_eq!(
            required_model_capabilities(&input),
            vision | ModelCapabilities::DOCUMENT_INPUT
        );
        missing!(
            &input,
            ModelCapabilities::empty(),
            RequiredModelCapability::ToolUse,
            ModelRequestFeature::ToolHistory
        );
        missing!(
            &input,
            tool,
            RequiredModelCapability::Reasoning,
            ModelRequestFeature::ReasoningHistory
        );
        missing!(
            &input,
            reasoning,
            RequiredModelCapability::Vision,
            ModelRequestFeature::ToolResultImage
        );
        missing!(
            &input,
            vision,
            RequiredModelCapability::DocumentInput,
            ModelRequestFeature::ToolResultDocument
        );
    }

    #[test]
    fn tool_cache_hint_is_independently_gated() {
        let mut input = request();
        input.tools.push(tool(Some(CacheHint::Ephemeral)));
        missing!(
            &input,
            ModelCapabilities::TOOL_USE,
            RequiredModelCapability::PromptCaching,
            ModelRequestFeature::ToolCacheHint
        );
    }
}
