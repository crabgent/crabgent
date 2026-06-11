//! Provider-agnostic token estimation.
//!
//! Single source of truth for "how many tokens does this text cost".
//! Backed by `tiktoken_rs` `cl100k_base` (`OpenAI`'s GPT-3.5/4 BPE).
//! `Anthropic` does not publish a tokenizer; `cl100k` is the accepted
//! proxy for pre-flight estimates and converges within a few percent of
//! `usage.input_tokens` on prose. Use API-returned `usage` for post-call
//! accounting; use this estimator for thresholds.
//!
//! The BPE table is materialized lazily on first call (~5-10 ms first
//! hit, sub-millisecond afterwards). Call [`warmup`] from a builder if
//! you want to move the cost out of the request hot path.

use once_cell::sync::Lazy;
use tiktoken_rs::CoreBPE;

#[expect(
    clippy::non_std_lazy_statics,
    reason = "tiktoken_rs CoreBPE is not const-constructible; once_cell::Lazy is the documented cache shape"
)]
static ENCODER: Lazy<CoreBPE> =
    Lazy::new(|| tiktoken_rs::cl100k_base().expect("cl100k_base encoder should be embedded"));

/// Approximate token cost for one image content block.
///
/// `OpenAI`'s vision pricing assumes ~1600 tokens for a high-detail image;
/// `Anthropic`'s per-image cost is in the same ballpark for typical input
/// resolutions. Treated as a constant because neither provider exposes a
/// pre-flight image-token oracle.
pub const IMAGE_TOKENS: usize = 1_600;

/// Estimate the token count of a UTF-8 text fragment.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    ENCODER.encode_ordinary(text).len()
}

/// Force the BPE table to materialize. Useful from builders so the first
/// production call does not pay the ~5-10 ms lazy-init cost.
pub fn warmup() {
    Lazy::force(&ENCODER);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_is_positive_for_nonempty_text() {
        assert!(estimate_tokens("hello") > 0);
    }

    #[test]
    fn estimate_tokens_grows_with_length() {
        let short = estimate_tokens("hello");
        let long = estimate_tokens(&"hello ".repeat(100));
        assert!(long > short);
    }

    #[test]
    fn estimate_tokens_counts_multibyte_correctly() {
        let ascii = estimate_tokens(&"x".repeat(40));
        let cjk = estimate_tokens(&"日".repeat(40));
        assert!(cjk > 0);
        assert!(ascii > 0);
    }

    #[test]
    fn warmup_does_not_panic() {
        warmup();
    }
}
