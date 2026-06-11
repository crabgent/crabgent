//! [`RecallHandle`]: the content-addressed key for a stashed full output.
//!
//! Shape: `{run_id}.{sha256_hex_prefix}`. The run id prefix lets
//! [`crate::recall::RecallTool`] recover the originating run for auto-disable
//! accounting even though `ToolCtx` carries no run id. The hash prefix makes
//! the handle content-addressed, so re-stashing identical output is
//! idempotent against the store's `ON CONFLICT DO NOTHING`.

use std::fmt;
use std::str::FromStr;

use crabgent_core::run_id::{ParseRunIdError, RunId};
use sha2::{Digest, Sha256};

/// Number of hex characters kept from the SHA-256 digest (64 bits).
const HASH_HEX_LEN: usize = 16;

/// A recovery handle for a stashed tool output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallHandle {
    run_id: RunId,
    hash: String,
}

impl RecallHandle {
    /// Derive a handle from the originating run and the full output content.
    #[must_use]
    pub fn new(run_id: &RunId, content: &str) -> Self {
        let digest = Sha256::digest(content.as_bytes());
        Self {
            run_id: run_id.clone(),
            hash: hex_prefix(&digest),
        }
    }

    /// The run that produced the stashed output.
    #[must_use]
    pub const fn run_id(&self) -> &RunId {
        &self.run_id
    }
}

impl fmt::Display for RecallHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.run_id, self.hash)
    }
}

/// Error returned by [`RecallHandle::from_str`].
#[derive(Debug, thiserror::Error)]
pub enum ParseHandleError {
    /// No `.` separating the run id from the hash prefix.
    #[error("recall handle missing '.' separator")]
    MissingSeparator,
    /// The run id segment is not a valid [`RunId`].
    #[error("recall handle run id invalid: {0}")]
    RunId(#[from] ParseRunIdError),
    /// The hash segment is not the expected hex prefix.
    #[error("recall handle hash segment must be {HASH_HEX_LEN} hex chars")]
    Hash,
}

impl FromStr for RecallHandle {
    type Err = ParseHandleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (run, hash) = s
            .split_once('.')
            .ok_or(ParseHandleError::MissingSeparator)?;
        let run_id = RunId::from_str(run)?;
        if hash.len() != HASH_HEX_LEN || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ParseHandleError::Hash);
        }
        Ok(Self {
            run_id,
            hash: hash.to_owned(),
        })
    }
}

/// Hex-encode the first `HASH_HEX_LEN / 2` bytes of a digest.
fn hex_prefix(digest: &[u8]) -> String {
    let mut out = String::with_capacity(HASH_HEX_LEN);
    for &byte in digest.iter().take(HASH_HEX_LEN / 2) {
        out.push(hex_nibble(byte >> 4));
        out.push(hex_nibble(byte & 0x0f));
    }
    out
}

/// Map a 4-bit value to its lowercase hex digit.
const fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'a' + n - 10) as char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_is_deterministic_and_content_addressed() {
        let run = RunId::new();
        let a = RecallHandle::new(&run, "hello world");
        let b = RecallHandle::new(&run, "hello world");
        let c = RecallHandle::new(&run, "different");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn handle_derive_parse_roundtrip() {
        let run = RunId::new();
        let handle = RecallHandle::new(&run, "payload");
        let text = handle.to_string();
        let parsed: RecallHandle = text.parse().expect("roundtrip");
        assert_eq!(handle, parsed);
        assert_eq!(parsed.run_id(), &run);
    }

    #[test]
    fn display_has_run_id_dot_hash_shape() {
        let run = RunId::new();
        let text = RecallHandle::new(&run, "x").to_string();
        let (run_seg, hash_seg) = text.split_once('.').expect("separator");
        assert_eq!(run_seg, run.to_string());
        assert_eq!(hash_seg.len(), HASH_HEX_LEN);
        assert!(hash_seg.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn parse_rejects_missing_separator() {
        let err = "no-dot-here".parse::<RecallHandle>().expect_err("error");
        assert!(matches!(err, ParseHandleError::MissingSeparator));
    }

    #[test]
    fn parse_rejects_bad_run_id() {
        let err = "not-a-uuid.0011223344556677"
            .parse::<RecallHandle>()
            .expect_err("error");
        assert!(matches!(err, ParseHandleError::RunId(_)));
    }

    #[test]
    fn parse_rejects_bad_hash() {
        let run = RunId::new();
        let bad = format!("{run}.zzzz");
        let err = bad.parse::<RecallHandle>().expect_err("error");
        assert!(matches!(err, ParseHandleError::Hash));
    }
}
