//! Typed `POST /v1/responses` request, response, and streaming surface.

mod stream;
mod types;

pub use stream::{ResponseStreamEvent, ResponsesStream};
pub use types::{
    CreateResponseRequest, IncompleteDetails, MessageContent, MessageOutput, ReasoningItem,
    Response, ResponseError, ResponseFunctionTool, ResponseInputContent, ResponseInputItem,
    ResponseJsonSchemaFormat, ResponseOutputItem, ResponseReasoning, ResponseTextConfig,
    ResponseUsage,
};

use crate::api::{OpenAiClient, OpenAiError};

impl OpenAiClient {
    pub async fn responses_stream(
        &self,
        request: &CreateResponseRequest,
    ) -> Result<ResponsesStream, OpenAiError> {
        let response = self.post("v1/responses", request).await?;
        Ok(ResponsesStream::new(response))
    }
}
