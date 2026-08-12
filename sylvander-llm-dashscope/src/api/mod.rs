//! Direct typed support for native `DashScope` Generation.

mod client;
mod error;
mod sse;
mod stream;
mod types;

pub use client::DashScopeClient;
pub use error::DashScopeError;
pub use stream::{GenerationStream, GenerationStreamEvent};
pub use types::{
    GenerationChoice, GenerationCompletion, GenerationFunctionCallParam,
    GenerationFunctionDefinition, GenerationFunctionTool, GenerationInput, GenerationMessageParam,
    GenerationOutput, GenerationParameters, GenerationRequest, GenerationResponse,
    GenerationToolCall, GenerationToolCallParam, GenerationToolKind, GenerationUsage,
};

impl DashScopeClient {
    pub async fn generation_stream(
        &self,
        request: &GenerationRequest,
    ) -> Result<GenerationStream, DashScopeError> {
        let response = self.post(request).await?;
        Ok(GenerationStream::new(response))
    }
}
