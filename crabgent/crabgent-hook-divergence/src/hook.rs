//! [`DivergenceHook`]: route high-confidence text-vs-prosody divergence to a
//! one-shot audio perception call, tag the turn, and log a corpus row.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_channel::AudioStore;
use crabgent_core::{AudioRef, Decision, Hook, LlmRequest, ModelId, Outcome, Provider, RunCtx};
use crabgent_prosody::{DivergenceConfidence, DivergenceDetector, DivergenceVerdict};
use crabgent_store::MemoryStore;
use crabgent_tool_audio::{AudioCall, AudioCircuit, ask_audio};

use crate::cache::{CachedVerdict, PerceptionCache};
use crate::corpus::write_event;
use crate::render::{render_perception_block, strip_prior_perception};
use crate::scan::{find_latest_transcript, is_current_turn, push_block};

/// The single question put to the audio model on a flagged divergence.
const DIVERGENCE_QUESTION: &str = "The transcript words and the speaker's tone \
    may conflict. In one short sentence, describe the speaker's actual emotional \
    tone and whether it contradicts the literal words (for example sarcasm, \
    irony, or bitterness).";

/// Hook that detects text-vs-prosody divergence and, on a high-confidence flag,
/// pushes a one-shot audio perception call, tags the turn, and logs a corpus
/// row.
///
/// Holds the cheap detector plus the dependencies the push path needs: the
/// retained-audio store, the `[audio]` route (`provider` + `model`, resolved
/// independently of the chat model), and the corpus store. All injected by the
/// consumer; `RunCtx` carries none of them.
pub struct DivergenceHook {
    detector: DivergenceDetector,
    store: Arc<dyn AudioStore>,
    provider: Arc<dyn Provider>,
    model: ModelId,
    memory: Arc<dyn MemoryStore>,
    circuit: Arc<AudioCircuit>,
    cache: PerceptionCache,
}

impl DivergenceHook {
    /// Wire the hook with the detector, the retained-audio store, the `[audio]`
    /// route (`provider` + `model`), the corpus store, and the shared
    /// [`AudioCircuit`].
    ///
    /// The circuit is shared (`Arc`) with the `hear_again` pull tool so the
    /// per-call timeout, the breaker, and the send-byte cap bound every audio
    /// call across both paths.
    #[must_use]
    pub fn new(
        detector: DivergenceDetector,
        store: Arc<dyn AudioStore>,
        provider: Arc<dyn Provider>,
        model: ModelId,
        memory: Arc<dyn MemoryStore>,
        circuit: Arc<AudioCircuit>,
    ) -> Self {
        Self {
            detector,
            store,
            provider,
            model,
            memory,
            circuit,
            cache: PerceptionCache::new(),
        }
    }

    /// Resolve the tone for `(run, audio_ref)`, routing the audio call at most
    /// once per run. A cache hit re-injects the prior verdict without a second
    /// call; a fresh route writes the corpus row when it yields a tone.
    async fn tone_for(
        &self,
        audio_ref: &str,
        ctx: &RunCtx,
        text: &str,
        verdict: DivergenceVerdict,
    ) -> Option<String> {
        match self.cache.get(&ctx.run_id, audio_ref) {
            Some(CachedVerdict::Tone(tone)) => return Some(tone),
            Some(CachedVerdict::Negative) => return None,
            None => {}
        }
        let tone = self.push_audio(audio_ref, ctx).await;
        let cached = tone.as_ref().map_or(CachedVerdict::Negative, |tone| {
            CachedVerdict::Tone(tone.clone())
        });
        self.cache
            .put(ctx.run_id.clone(), audio_ref.to_owned(), cached);
        if let Some(tone) = tone.as_deref() {
            write_event(
                self.memory.as_ref(),
                &ctx.subject,
                text,
                verdict,
                audio_ref,
                tone,
            )
            .await;
        }
        tone
    }

    /// Run the speculative audio call under the shared circuit (per-call
    /// timeout, breaker, and send-byte cap). Returns the tone read, or `None`
    /// on any failure (fail-open). Errors are logged for the operator, never
    /// surfaced to the LLM.
    async fn push_audio(&self, audio_ref: &str, ctx: &RunCtx) -> Option<String> {
        let handle = AudioRef::new(audio_ref);
        let call = ask_audio(AudioCall {
            store: self.store.as_ref(),
            provider: self.provider.as_ref(),
            model: self.model.clone(),
            audio_ref: &handle,
            question: DIVERGENCE_QUESTION.to_owned(),
            subject: ctx.subject.clone(),
            cancel: Some(&ctx.cancel),
            max_send_bytes: self.circuit.max_send_bytes(),
        });
        match self.circuit.run(call).await {
            Ok(answer) => Some(answer.answer),
            Err(error) => skip(&error),
        }
    }
}

/// Log a fail-open skip once and yield `None`, so the turn proceeds on the plain
/// transcript. Centralising the single `warn!` site keeps `push_audio` under the
/// cognitive-complexity cap: the `crabgent_log` macro expands into several
/// branches under workspace feature unification, so two inline sites breach 15.
fn skip(detail: &dyn std::fmt::Display) -> Option<String> {
    crabgent_log::warn!(
        hook = "divergence",
        detail = %detail,
        "divergence audio push skipped, using plain transcript"
    );
    None
}

#[async_trait]
impl Hook for DivergenceHook {
    async fn before_llm(&self, req: &LlmRequest, ctx: &RunCtx) -> Decision<LlmRequest> {
        let mut next = req.clone();
        let stripped = strip_prior_perception(&mut next.messages);

        let Some(view) = find_latest_transcript(&next.messages) else {
            return decide(stripped, next);
        };
        // Correlation + TTL: only the current turn's transcript routes. A stale
        // prior-turn transcript (a newer user turn came after) is dropped so it
        // cannot re-trigger the audio call every later turn.
        if !is_current_turn(&next.messages, view.message_index) {
            return decide(stripped, next);
        }
        let verdict = self.detector.detect(&view.text, &view.voice);
        if !(verdict.diverges && verdict.confidence == DivergenceConfidence::High) {
            return decide(stripped, next);
        }
        let Some(audio_ref) = view.audio_ref else {
            return decide(stripped, next);
        };
        let Some(tone) = self.tone_for(&audio_ref, ctx, &view.text, verdict).await else {
            return decide(stripped, next);
        };

        let added = push_block(
            &mut next.messages,
            view.message_index,
            render_perception_block(&tone),
        );
        decide(stripped || added, next)
    }

    async fn on_stop(&self, ctx: &RunCtx, _outcome: &Outcome) {
        // Reclaim this run's cached verdicts deterministically, so a finished
        // run's entries do not linger until FIFO eviction (which could drop a
        // live run's entry first). Mirrors the per-run cleanup the sibling
        // CompactHook/ToolCompactHook do on stop.
        self.cache.clear_run(&ctx.run_id);
    }
}

/// `Replace` when the request changed, else `Continue`.
fn decide(changed: bool, next: LlmRequest) -> Decision<LlmRequest> {
    if changed {
        Decision::Replace(next)
    } else {
        Decision::Continue
    }
}
