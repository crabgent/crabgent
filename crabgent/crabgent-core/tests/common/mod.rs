use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crabgent_core::{
    LlmRequest, LlmResponse, ModelInfo, Provider, ProviderCapabilities, ProviderError, RunCtx,
};

mod noop_tool;

pub use crabgent_test_support::done;
use crabgent_test_support::{tool_call, tool_use};
pub use noop_tool::NoopTool;

pub struct RecordingProvider {
    responses: Mutex<Vec<LlmResponse>>,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
}

impl RecordingProvider {
    pub const fn with(responses: Vec<LlmResponse>, requests: Arc<Mutex<Vec<LlmRequest>>>) -> Self {
        Self {
            responses: Mutex::new(responses),
            requests,
        }
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,

        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.requests
            .lock()
            .expect("requests mutex not poisoned")
            .push(req.clone());
        let mut responses = self.responses.lock().expect("responses mutex not poisoned");
        if responses.is_empty() {
            return Err(ProviderError::Other("script exhausted".into()));
        }
        Ok(responses.remove(0))
    }

    fn name(&self) -> &'static str {
        "recording"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..ProviderCapabilities::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("m", "recording")]
    }
}

pub fn calling_tool(name: &str, id: &str, args: Value) -> LlmResponse {
    tool_use(vec![tool_call(id, name, args)])
}
