//! Typed `POST /v1/chat/completions` streaming surface.

mod stream;
mod types;

pub use stream::{ChatCompletionStream, ChatStreamEvent};
pub use types::{
    ChatCompletion, ChatCompletionChunk, ChatCompletionUsage, ChatFunction, ChatFunctionDefinition,
    ChatFunctionTool, ChatImageUrl, ChatJsonSchema, ChatMessageParam, ChatResponseFormat,
    ChatStreamOptions, ChatToolCall, ChatToolCallParam, ChatUserContentPart,
    CreateChatCompletionRequest,
};

use crate::api::{OpenAiClient, OpenAiError};

impl OpenAiClient {
    pub async fn chat_completions_stream(
        &self,
        request: &CreateChatCompletionRequest,
    ) -> Result<ChatCompletionStream, OpenAiError> {
        let response = self.post("v1/chat/completions", request).await?;
        Ok(ChatCompletionStream::new(response))
    }
}
