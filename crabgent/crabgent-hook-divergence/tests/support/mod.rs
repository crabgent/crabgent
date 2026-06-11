//! Shared fixtures for the [`DivergenceHook`] integration tests: a stub audio
//! store, a call-counting audio provider with selectable behavior, request and
//! voice builders, and corpus-read helpers.

#![allow(
    dead_code,
    reason = "integration test helper module is compiled separately by each test binary"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use crabgent_channel::{AudioStore, AudioStoreError};
use crabgent_core::{
    AudioRef, Decision, LlmRequest, LlmResponse, MemoryScope, ModelId, Owner, Provider,
    ProviderCapabilities, ProviderError, RunCtx, RunId, SearchQuery, StopReason, Subject, Usage,
    WebSearchConfig,
};
use crabgent_hook_divergence::DivergenceHook;
use crabgent_prosody::DivergenceDetector;
use crabgent_store::{MemoryDoc, MemoryMemoryStore, MemoryStore};
use crabgent_tool_audio::{AudioCircuit, AudioCircuitConfig};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

pub const AUDIO_MODEL: &str = "gpt-4o-audio-preview";
pub const CHAT_MODEL: &str = "gpt-4o";
pub const SPEAKER: &str = "u-speaker";

pub struct ReadyStore;

#[async_trait]
impl AudioStore for ReadyStore {
    async fn put(&self, _bytes: Bytes, _mime: &str) -> Result<AudioRef, AudioStoreError> {
        Ok(AudioRef::new("stub-put"))
    }

    async fn get(&self, _audio_ref: &AudioRef) -> Result<(Bytes, String), AudioStoreError> {
        Ok((
            Bytes::from_static(b"RIFFstub-audio"),
            "audio/wav".to_owned(),
        ))
    }
}

pub enum Behavior {
    Answer(&'static str),
    Fail,
    Slow(Duration),
}

pub struct CountingProvider {
    calls: Arc<AtomicUsize>,
    behavior: Behavior,
}

impl CountingProvider {
    pub fn new(behavior: Behavior) -> (Arc<Self>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                calls: calls.clone(),
                behavior,
            }),
            calls,
        )
    }
}

#[async_trait]
impl Provider for CountingProvider {
    async fn complete(
        &self,
        _req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behavior {
            Behavior::Answer(text) => Ok(response(text)),
            Behavior::Fail => Err(ProviderError::Transport("audio backend down".into())),
            Behavior::Slow(delay) => {
                tokio::time::sleep(delay).await;
                Ok(response("too late"))
            }
        }
    }

    fn name(&self) -> &'static str {
        "counting"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            audio: true,
            ..Default::default()
        }
    }
}

fn response(text: &str) -> LlmResponse {
    LlmResponse {
        text: text.to_owned(),
        tool_calls: Vec::new(),
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        },
        model: ModelId::from(AUDIO_MODEL),
    }
}

pub fn request_with(text: &str, voice: Value, audio_ref: &str) -> LlmRequest {
    let mut block = json!({"type": "transcript", "text": text, "source_audio": audio_ref});
    block["voice"] = voice;
    LlmRequest {
        model: ModelId::from(CHAT_MODEL),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": [block]})],
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}

/// A flat, low-energy delivery: slow rate plus a long pause.
pub fn flat_voice() -> Value {
    json!({"pause_ms": 1200, "speech_rate_wpm": 80, "hesitation_count": 1})
}

/// An animated, high-energy delivery: fast rate plus laughter.
pub fn animated_voice() -> Value {
    json!({"speech_rate_wpm": 230, "audio_events": [{"label": "laughter"}]})
}

pub fn default_circuit() -> Arc<AudioCircuit> {
    Arc::new(AudioCircuit::new(AudioCircuitConfig::default()))
}

pub fn hook_with(
    provider: Arc<CountingProvider>,
    memory: Arc<MemoryMemoryStore>,
    circuit: Arc<AudioCircuit>,
) -> DivergenceHook {
    DivergenceHook::new(
        DivergenceDetector::default(),
        Arc::new(ReadyStore),
        provider,
        ModelId::from(AUDIO_MODEL),
        memory,
        circuit,
    )
}

pub fn hook(provider: Arc<CountingProvider>, memory: Arc<MemoryMemoryStore>) -> DivergenceHook {
    hook_with(provider, memory, default_circuit())
}

pub fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new(SPEAKER)).with_cancel(CancellationToken::new())
}

pub fn replaced(decision: Decision<LlmRequest>) -> Option<LlmRequest> {
    match decision {
        Decision::Replace(req) => Some(req),
        _ => None,
    }
}

pub fn perception_block(req: &LlmRequest) -> Option<String> {
    req.messages
        .iter()
        .filter_map(|msg| msg.get("content").and_then(Value::as_array))
        .flatten()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .find(|text| text.starts_with("<perception crabgent=\"1\""))
        .map(str::to_owned)
}

pub async fn corpus_docs(memory: &MemoryMemoryStore) -> Vec<MemoryDoc> {
    let scope = MemoryScope::for_owner(Owner::new(SPEAKER));
    let hits = memory
        .search(&SearchQuery::new("").scope(scope))
        .await
        .expect("search ok");
    let mut docs = Vec::with_capacity(hits.len());
    for hit in hits {
        docs.push(
            memory
                .get(&hit.id)
                .await
                .expect("get ok")
                .expect("doc present"),
        );
    }
    docs
}
