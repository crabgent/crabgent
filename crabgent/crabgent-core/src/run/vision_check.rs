//! Vision capability pre-flight before provider calls.

use serde_json::Value;

use crate::error::ProviderError;
use crate::model::{ModelRegistry, ModelTarget};
use crate::provider::Provider;
use crate::types::LlmRequest;

pub(super) fn check_vision_capability(
    req: &LlmRequest,
    provider: &dyn Provider,
    registry: &ModelRegistry,
) -> Result<(), ProviderError> {
    if !contains_image(&req.messages) {
        return Ok(());
    }

    if provider.capabilities().vision && model_supports_vision(req, provider, registry) {
        return Ok(());
    }

    Err(ProviderError::VisionUnsupported {
        provider: provider.name().into(),
        model: req.model.as_str().into(),
    })
}

fn model_supports_vision(
    req: &LlmRequest,
    provider: &dyn Provider,
    registry: &ModelRegistry,
) -> bool {
    let target = ModelTarget::new(provider.name(), req.model.clone());
    registry
        .get_target(&target)
        .is_some_and(|info| info.caps.supports_vision)
}

fn contains_image(messages: &[Value]) -> bool {
    messages.iter().any(message_contains_image)
}

fn message_contains_image(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|content| content.iter().any(is_image_block))
}

fn is_image_block(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("image")
}
