//! Build and persist a personal divergence-corpus row.
//!
//! Each high-confidence divergence becomes one durable, speaker-scoped memory:
//! the user-owned local sarcasm/irony corpus. Writing is best-effort and never
//! aborts the turn.

use crabgent_core::text::truncate_with_ellipsis;
use crabgent_core::{MemoryScope, Owner, Subject};
use crabgent_prosody::DivergenceVerdict;
use crabgent_store::{MemoryDoc, MemoryStore};

/// Memory class for corpus rows. A durable, non-decaying class (the stored
/// string of `crabgent_memory::MemoryClass::Notes`), written directly so this
/// crate needs no dependency on crabgent-memory.
const CORPUS_CLASS: &str = "notes";

/// Importance assigned to a divergence-corpus row.
const CORPUS_IMPORTANCE: f32 = 0.7;

/// First line of every corpus body, so rows are distinguishable and searchable.
const CORPUS_MARKER: &str = "# divergence_event";

/// Maximum transcript bytes embedded in the corpus body.
const TRANSCRIPT_CAP: usize = 2000;

/// Maximum handle bytes embedded in the corpus body. A real handle is a short
/// UUID; the cap matches the other untrusted fields so no path can write an
/// unbounded value into the body.
const AUDIO_REF_CAP: usize = 256;

/// Maximum tone bytes embedded in the corpus body. The tone is untrusted audio
/// model output capped at 8KiB upstream; the durable corpus row caps it again so
/// no field can write an unbounded value into the body, matching the render
/// path's `TONE_ATTR_CAP`.
const CORPUS_TONE_CAP: usize = 512;

/// Persist one divergence event to the speaker-scoped corpus. Best-effort: a
/// store error is logged, never propagated, so the turn is never aborted.
pub async fn write_event(
    memory: &dyn MemoryStore,
    subject: &Subject,
    text: &str,
    verdict: DivergenceVerdict,
    audio_ref: &str,
    tone: &str,
) {
    let scope = MemoryScope::for_owner(Owner::new(subject.id()));
    let mut doc = MemoryDoc::new(scope, corpus_body(text, verdict, audio_ref, tone));
    doc.class = Some(CORPUS_CLASS.to_owned());
    doc.importance = Some(CORPUS_IMPORTANCE);
    if let Err(error) = memory.store(&doc).await {
        crabgent_log::warn!(
            hook = "divergence",
            subject = subject.id(),
            %error,
            "divergence corpus write failed"
        );
    }
}

/// Render the corpus body. The audio verdict (`tone`) is the audio model's own
/// output stored verbatim in this operator-owned local corpus; the prompt-side
/// `<perception>` tag is what escapes it before it reaches the LLM.
fn corpus_body(text: &str, verdict: DivergenceVerdict, audio_ref: &str, tone: &str) -> String {
    let transcript = truncate_with_ellipsis(text, TRANSCRIPT_CAP, " ...").into_owned();
    let audio_ref = truncate_with_ellipsis(audio_ref, AUDIO_REF_CAP, "").into_owned();
    let tone = truncate_with_ellipsis(tone, CORPUS_TONE_CAP, "").into_owned();
    format!(
        "{CORPUS_MARKER}\n\
         transcript: {transcript}\n\
         text_polarity: {:?}\n\
         prosody: {:?}\n\
         confidence: {:?}\n\
         audio_ref: {audio_ref}\n\
         audio_verdict: {tone}",
        verdict.text_polarity, verdict.prosody, verdict.confidence,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crabgent_prosody::{DivergenceConfidence, ProsodyEnergy, TextPolarity};

    fn verdict() -> DivergenceVerdict {
        DivergenceVerdict {
            diverges: true,
            confidence: DivergenceConfidence::High,
            prosody: ProsodyEnergy::Flat,
            text_polarity: TextPolarity::Positive,
        }
    }

    #[test]
    fn body_carries_all_fields() {
        let body = corpus_body("ja super", verdict(), "aud-1", "flat, sarcastic");
        assert!(body.starts_with(CORPUS_MARKER));
        assert!(body.contains("transcript: ja super"));
        assert!(body.contains("text_polarity: Positive"));
        assert!(body.contains("prosody: Flat"));
        assert!(body.contains("confidence: High"));
        assert!(body.contains("audio_ref: aud-1"));
        assert!(body.contains("audio_verdict: flat, sarcastic"));
    }

    #[test]
    fn long_transcript_is_bounded() {
        let body = corpus_body(&"x".repeat(5000), verdict(), "a", "t");
        assert!(body.len() < 5000, "transcript bounded: {}", body.len());
    }

    #[test]
    fn long_tone_is_bounded() {
        // The audio verdict is untrusted model output; the durable corpus row
        // must cap it like the render path does, not embed it raw.
        let tone = "t".repeat(5000);
        let body = corpus_body("ja super", verdict(), "a", &tone);
        let stored = body
            .split_once("audio_verdict: ")
            .map(|(_, rest)| rest)
            .expect("audio_verdict field");
        assert!(
            stored.len() <= CORPUS_TONE_CAP,
            "tone bounded: {}",
            stored.len()
        );
    }
}
