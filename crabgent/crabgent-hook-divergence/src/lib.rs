//! # crabgent-hook-divergence
//!
//! The divergence-routing path of the voice-perception epic. [`DivergenceHook`]
//! runs in `before_llm`: it reads the latest user transcript, asks the cheap
//! local [`crabgent_prosody::DivergenceDetector`] whether the words contradict
//! the prosodic delivery, and only on a high-confidence contradiction does it
//! spend money. On that flag it `PUSHes` a one-shot audio-native perception call
//! (the same `crabgent_tool_audio::ask_audio` path the `hear_again` pull tool
//! uses, never a parallel mechanism), appends a trust-fenced
//! `<perception conflict="text-vs-prosody" .../>` block, and logs the event to
//! a personal divergence corpus scoped to the speaker.
//!
//! The hook is fail-open: a detector miss, a stale prior-turn transcript, a
//! missing handle, an audio-call error, an open circuit, or a per-call timeout
//! all degrade to the plain transcript with no tag and no corpus row. It never
//! blocks the turn and never panics. The emitted tag is its own separate text
//! block with its own sentinel, disjoint from the `<voice>` tag
//! (`crabgent_prosody::ProsodyHook`, prepended into the transcript text) and the
//! `[crabgent:audio-hint]` block (`crabgent_tool_audio`), so the three
//! annotations compose in any hook order without colliding.
//!
//! Hardening design hardening: the audio call runs under the shared
//! [`crabgent_tool_audio::AudioCircuit`] (per-call timeout + consecutive-failure
//! breaker + send-byte cap), the verdict is bound to its source `(RunId,
//! AudioRef)` so a stale or repeated transcript routes the call at most once per
//! run, and only the current turn's transcript routes.

#![forbid(unsafe_code)]

mod cache;
mod corpus;
mod hook;
mod render;
mod scan;

pub use hook::DivergenceHook;
