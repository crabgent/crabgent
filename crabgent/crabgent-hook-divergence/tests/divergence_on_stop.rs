//! `on_stop` reclaims a finished run's [`PerceptionCache`] entries so a later
//! turn re-routes the audio call instead of re-using a stale verdict.

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crabgent_core::{Hook, Outcome};
use crabgent_store::MemoryMemoryStore;
use support::{Behavior, CountingProvider, ctx, flat_voice, hook, replaced, request_with};

#[tokio::test]
async fn cache_cleared_on_stop() {
    // on_stop reclaims the run's cached verdict, so a later before_llm over the
    // same (run, audio_ref) routes the audio call again instead of re-using a
    // stale cache entry. Without the clear, the second pass would re-inject the
    // cached tag and the call count would stay at 1.
    let (provider, calls) = CountingProvider::new(Behavior::Answer("flat, bitter delivery"));
    let memory = Arc::new(MemoryMemoryStore::default());
    let hook = hook(provider, memory.clone());
    let run_ctx = ctx();
    let req = request_with("ja super", flat_voice(), "aud-1");

    let first = hook.before_llm(&req, &run_ctx).await;
    assert!(replaced(first).is_some(), "first pass tags the turn");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "routed once");

    hook.on_stop(&run_ctx, &Outcome::Completed(String::new()))
        .await;

    let second = hook.before_llm(&req, &run_ctx).await;
    assert!(
        replaced(second).is_some(),
        "post-clear pass re-tags the turn"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the cleared run re-routes the audio call"
    );
}
