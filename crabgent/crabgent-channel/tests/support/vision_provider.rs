use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crabgent_core::error::ProviderError;
use crabgent_core::model::ModelInfo;
use crabgent_core::provider::{Provider, ProviderCapabilities};
use crabgent_core::types::{LlmRequest, LlmResponse, StopReason, Usage};

#[derive(Clone)]
pub struct RecordingProvider {
    captured: Arc<Mutex<Vec<LlmRequest>>>,
    model_id: &'static str,
    provider_name: &'static str,
    supports_vision: bool,
    supports_audio: bool,
}

impl RecordingProvider {
    #[must_use]
    pub fn with_caps(
        model_id: &'static str,
        provider_name: &'static str,
        supports_vision: bool,
        supports_audio: bool,
    ) -> Self {
        Self {
            captured: Arc::new(Mutex::new(Vec::new())),
            model_id,
            provider_name,
            supports_vision,
            supports_audio,
        }
    }

    #[must_use]
    pub fn captured(&self) -> Vec<LlmRequest> {
        self.captured
            .lock()
            .expect("request capture lock not poisoned")
            .clone()
    }
}

#[async_trait]
impl Provider for RecordingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &crabgent_core::RunCtx,
        _cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.captured
            .lock()
            .expect("request capture lock not poisoned")
            .push(req.clone());
        Ok(LlmResponse {
            text: "ok".to_owned(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: req.model.clone(),
        })
    }

    fn name(&self) -> &'static str {
        self.provider_name
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            vision: self.supports_vision,
            audio: self.supports_audio,
            ..Default::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        let mut info = ModelInfo::minimal(self.model_id, self.provider_name);
        info.caps.supports_vision = self.supports_vision;
        info.caps.supports_audio = self.supports_audio;
        vec![info]
    }
}
