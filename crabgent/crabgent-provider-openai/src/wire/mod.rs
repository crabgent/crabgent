//! Wire-format abstraction for OpenAI-compatible endpoints.

use std::any::Any;

use crabgent_core::{LlmRequest, LlmResponse, ProviderEvent, RunCtx};
use serde_json::Value;

use crate::types::OpenAiError;

pub mod chat_completions;
pub mod responses;

/// Compile-time wire-format surface for one endpoint family.
pub trait WireFormat: Send + Sync {
    type StreamState: Default + Send + Sync + 'static;

    fn endpoint_path(&self) -> &str;
    fn build_body(
        &self,
        req: &LlmRequest,
        ctx: &RunCtx,
        stream: bool,
    ) -> Result<Value, OpenAiError>;
    fn parse_response(&self, body: Value) -> Result<LlmResponse, OpenAiError>;
    fn parse_sse_event(&self, line: &str, state: &mut Self::StreamState) -> Option<ProviderEvent>;
}

/// Object-safe erasure layer used by `AuthStrategy`.
pub trait WireFormatDyn: Send + Sync {
    fn endpoint_path(&self) -> &str;
    fn build_body(
        &self,
        req: &LlmRequest,
        ctx: &RunCtx,
        stream: bool,
    ) -> Result<Value, OpenAiError>;
    fn parse_response(&self, body: Value) -> Result<LlmResponse, OpenAiError>;
    fn new_stream_state(&self) -> Box<dyn Any + Send + Sync>;
    fn parse_sse_event_dyn(&self, line: &str, state: &mut dyn Any) -> Option<ProviderEvent>;
    fn clone_box(&self) -> Box<dyn WireFormatDyn>;
}

impl<T> WireFormatDyn for T
where
    T: WireFormat + Clone + 'static,
{
    fn endpoint_path(&self) -> &str {
        WireFormat::endpoint_path(self)
    }

    fn build_body(
        &self,
        req: &LlmRequest,
        ctx: &RunCtx,
        stream: bool,
    ) -> Result<Value, OpenAiError> {
        WireFormat::build_body(self, req, ctx, stream)
    }

    fn parse_response(&self, body: Value) -> Result<LlmResponse, OpenAiError> {
        WireFormat::parse_response(self, body)
    }

    fn new_stream_state(&self) -> Box<dyn Any + Send + Sync> {
        Box::<T::StreamState>::default()
    }

    fn parse_sse_event_dyn(&self, line: &str, state: &mut dyn Any) -> Option<ProviderEvent> {
        let Some(state) = state.downcast_mut::<T::StreamState>() else {
            // A wire/state mismatch means the erased state was created by a
            // different `WireFormat`. The SSE event is dropped; log so the
            // wrong pairing is observable instead of the stream going dark.
            crabgent_log::warn!(
                expected_state = std::any::type_name::<T::StreamState>(),
                "openai wire: SSE state type mismatch, dropping event"
            );
            return None;
        };
        WireFormat::parse_sse_event(self, line, state)
    }

    fn clone_box(&self) -> Box<dyn WireFormatDyn> {
        Box::new(self.clone())
    }
}
