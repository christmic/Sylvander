//! Direct typed support for native `DashScope` Generation.

mod client;
mod error;
mod sse;
mod stream;
mod types;

use serde::Serialize;

pub use client::{DEFAULT_TIMEOUT, DashScopeClient};
pub use error::DashScopeError;
pub use stream::{GenerationStream, GenerationStreamEvent};
pub use types::{
    GenerationChoice, GenerationCompletion, GenerationFunctionCallParam,
    GenerationFunctionDefinition, GenerationFunctionTool, GenerationInput, GenerationMessageParam,
    GenerationOutput, GenerationParameters, GenerationRequest, GenerationResponse,
    GenerationToolCall, GenerationToolCallParam, GenerationToolKind, GenerationUsage,
    MultimodalContent, MultimodalGenerationInput, MultimodalGenerationRequest,
    MultimodalMessageParam,
};

impl DashScopeClient {
    pub async fn generation_stream<T: Serialize + ?Sized>(
        &self,
        request: &T,
    ) -> Result<GenerationStream, DashScopeError> {
        let response = self.post(request).await?;
        Ok(GenerationStream::new(response))
    }
}
