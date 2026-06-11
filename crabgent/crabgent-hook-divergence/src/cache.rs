//! Bounded per-`(RunId, AudioRef)` cache of divergence verdicts.
//!
//! `before_llm` fires once per LLM call, so an agentic turn with N tool-loop
//! iterations would otherwise re-route the same transcript's audio call N times.
//! The cache records the routed result keyed by `(run, audio_ref)`: a repeat
//! within the same run re-injects the cached tag without a second audio call,
//! and a cached miss (route failed, fail-open) is not retried for that turn.
//!
//! Bounded to [`CACHE_CAP`] entries (FIFO eviction) so a long-lived hook shared
//! across many runs cannot grow without limit. Each stored tone is already
//! bounded by the render layer's `TONE_ATTR_CAP`.

use std::collections::VecDeque;
use std::sync::Mutex;

use crabgent_core::RunId;

/// Maximum number of cached `(run, audio_ref)` verdicts.
const CACHE_CAP: usize = 128;

/// The routed outcome for a `(run, audio_ref)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CachedVerdict {
    /// The audio call returned a tone; re-inject this tag on a repeat.
    Tone(String),
    /// The route failed open (no tone); inject nothing on a repeat instead of
    /// retrying the call for the same turn.
    Negative,
}

struct Entry {
    run: RunId,
    audio_ref: String,
    verdict: CachedVerdict,
}

/// FIFO-bounded cache of routed divergence verdicts.
pub struct PerceptionCache {
    entries: Mutex<VecDeque<Entry>>,
}

impl PerceptionCache {
    /// An empty cache.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
        }
    }

    /// The cached verdict for `(run, audio_ref)`, or `None` when not cached.
    pub fn get(&self, run: &RunId, audio_ref: &str) -> Option<CachedVerdict> {
        let entries = self.entries.lock().expect("perception cache poisoned");
        entries
            .iter()
            .find(|entry| &entry.run == run && entry.audio_ref == audio_ref)
            .map(|entry| entry.verdict.clone())
    }

    /// Record the routed verdict for `(run, audio_ref)`, evicting the oldest
    /// entry when at capacity. A duplicate key is left as-is (idempotent).
    pub fn put(&self, run: RunId, audio_ref: String, verdict: CachedVerdict) {
        let mut entries = self.entries.lock().expect("perception cache poisoned");
        if entries
            .iter()
            .any(|entry| entry.run == run && entry.audio_ref == audio_ref)
        {
            return;
        }
        if entries.len() >= CACHE_CAP {
            entries.pop_front();
        }
        entries.push_back(Entry {
            run,
            audio_ref,
            verdict,
        });
    }

    /// Drop every entry for `run`. Called on run end so a finished run's
    /// verdicts are reclaimed deterministically instead of waiting for FIFO
    /// eviction, which could otherwise drop a live run's entry early.
    pub fn clear_run(&self, run: &RunId) {
        let mut entries = self.entries.lock().expect("perception cache poisoned");
        entries.retain(|entry| &entry.run != run);
    }
}

impl Default for PerceptionCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{CACHE_CAP, CachedVerdict, PerceptionCache};
    use crabgent_core::RunId;

    fn tone(value: &str) -> CachedVerdict {
        CachedVerdict::Tone(value.to_owned())
    }

    #[test]
    fn miss_then_hit() {
        let cache = PerceptionCache::new();
        let run = RunId::new();
        assert_eq!(cache.get(&run, "aud-1"), None, "empty cache misses");
        cache.put(run.clone(), "aud-1".to_owned(), tone("flat"));
        assert_eq!(cache.get(&run, "aud-1"), Some(tone("flat")));
    }

    #[test]
    fn negative_result_is_cached() {
        let cache = PerceptionCache::new();
        let run = RunId::new();
        cache.put(run.clone(), "aud-1".to_owned(), CachedVerdict::Negative);
        assert_eq!(
            cache.get(&run, "aud-1"),
            Some(CachedVerdict::Negative),
            "fail-open is cached"
        );
    }

    #[test]
    fn distinct_run_or_ref_is_a_separate_key() {
        let cache = PerceptionCache::new();
        let run_a = RunId::new();
        let run_b = RunId::new();
        cache.put(run_a.clone(), "aud-1".to_owned(), tone("a"));
        assert_eq!(cache.get(&run_b, "aud-1"), None, "different run misses");
        assert_eq!(cache.get(&run_a, "aud-2"), None, "different ref misses");
    }

    #[test]
    fn duplicate_put_keeps_the_first_value() {
        let cache = PerceptionCache::new();
        let run = RunId::new();
        cache.put(run.clone(), "aud-1".to_owned(), tone("first"));
        cache.put(run.clone(), "aud-1".to_owned(), tone("second"));
        assert_eq!(cache.get(&run, "aud-1"), Some(tone("first")));
    }

    #[test]
    fn clear_run_drops_only_that_run() {
        let cache = PerceptionCache::new();
        let run_a = RunId::new();
        let run_b = RunId::new();
        cache.put(run_a.clone(), "aud-1".to_owned(), tone("a"));
        cache.put(run_b.clone(), "aud-1".to_owned(), tone("b"));
        cache.clear_run(&run_a);
        assert_eq!(cache.get(&run_a, "aud-1"), None, "cleared run dropped");
        assert_eq!(
            cache.get(&run_b, "aud-1"),
            Some(tone("b")),
            "other run retained"
        );
    }

    #[test]
    fn evicts_oldest_beyond_capacity() {
        let cache = PerceptionCache::new();
        let first = RunId::new();
        cache.put(first.clone(), "aud".to_owned(), tone("oldest"));
        for _ in 0..CACHE_CAP {
            cache.put(RunId::new(), "aud".to_owned(), tone("x"));
        }
        assert_eq!(cache.get(&first, "aud"), None, "oldest entry evicted");
    }
}
