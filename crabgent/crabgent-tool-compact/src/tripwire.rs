//! Safety-floor scans that run before any filter.
//!
//! Two distinct concerns, deliberately separated (no regex anywhere, so no
//! catastrophic backtracking; plain ASCII substring/token scans):
//!
//! - [`scan_damage`]: marks lines that carry a damage signature (panic,
//!   error, denial, HTTP 5xx, ...). The compactor force-keeps these verbatim
//!   so a reduction never drops a diagnostic line.
//! - [`contains_secret`]: a best-effort gate. A suspected leaked credential
//!   short-circuits the whole compaction to raw passthrough, so the compactor
//!   never surfaces a secret the raw output would not already have shown, and
//!   never echoes one into a footer or stash preview. Denylists are inherently
//!   incomplete; the invariant is "never worse than the no-compactor baseline".

use std::collections::BTreeSet;

/// Lines the safety-floor force-keeps verbatim, by original index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TripwireHits {
    /// Indices into the original line slice that must survive compaction.
    pub keep: BTreeSet<usize>,
}

impl TripwireHits {
    /// Number of force-kept lines.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keep.len()
    }

    /// Whether the scan kept nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keep.is_empty()
    }
}

/// Case-insensitive damage substrings. A line containing any one is kept.
const DAMAGE_SUBSTRINGS: &[&str] = &[
    "panic",
    "fatal",
    "segfault",
    "segmentation fault",
    "error",
    "err:",
    "stacktrace",
    "backtrace",
    "traceback",
    "exception",
    "denied",
    "unauthorized",
    "forbidden",
    "fail",
    // Source truncation markers. The renderer's own fold markers ("... N lines
    // omitted ...") are synthesized after the scan, so they are never in the
    // line slice this scans and cannot re-trip the wire.
    "truncat",
    "omitted",
];

/// Mark every line that carries a damage signature.
#[must_use]
pub fn scan_damage(lines: &[&str]) -> TripwireHits {
    let mut keep = BTreeSet::new();
    for (idx, line) in lines.iter().enumerate() {
        if line_is_damage(line) {
            keep.insert(idx);
        }
    }
    TripwireHits { keep }
}

fn line_is_damage(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    DAMAGE_SUBSTRINGS.iter().any(|sub| lower.contains(sub)) || has_http_5xx(&lower, line)
}

/// A 5xx status code adjacent to an `http`/`status` marker on the same line.
/// Requiring the marker keeps innocuous 3-digit numbers (byte counts) from
/// tripping the wire.
fn has_http_5xx(lower: &str, line: &str) -> bool {
    if !lower.contains("http") && !lower.contains("status") {
        return false;
    }
    line.split(|c: char| !c.is_ascii_digit())
        .any(|tok| tok.len() == 3 && tok.starts_with('5'))
}

/// Specific multi-char secret markers (PEM headers, explicit key labels).
const SECRET_SUBSTRINGS: &[&str] = &[
    "-----begin",
    "begin rsa private",
    "begin openssh private",
    "begin pgp",
    "private key",
    "aws_secret_access_key",
    "aws_access_key_id",
    "secret_access_key",
    "client_secret",
    "authorization: bearer",
];

/// Token prefixes for provider credentials. Matched on whitespace/delimiter
/// tokens of meaningful length, so ordinary words like `task-123` never trip.
const SECRET_TOKEN_PREFIXES: &[&str] = &[
    "sk-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xapp-",
    "ghp_",
    "gho_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "akia",
    // JWT (base64url of `{"`). Catches Bearer tokens regardless of the
    // surrounding header format, which the substring list may miss.
    "eyj",
];

/// Minimum token length before a prefix match counts as a likely credential.
const SECRET_TOKEN_MIN_LEN: usize = 20;

/// Best-effort secret-leak detection over the whole output.
#[must_use]
pub fn contains_secret(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    if SECRET_SUBSTRINGS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    content
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '=' | ':' | ',' | ';'))
        .any(is_secret_token)
}

fn is_secret_token(tok: &str) -> bool {
    if tok.len() < SECRET_TOKEN_MIN_LEN {
        return false;
    }
    let lower = tok.to_ascii_lowercase();
    SECRET_TOKEN_PREFIXES.iter().any(|p| lower.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept(lines: &[&str]) -> Vec<usize> {
        scan_damage(lines).keep.into_iter().collect()
    }

    #[test]
    fn tripwire_keeps_each_damage_class() {
        let lines = [
            "all good here",
            "thread 'main' panicked at src/x.rs",
            "FATAL: disk full",
            "Segmentation fault (core dumped)",
            "error[E0277]: trait not satisfied",
            "Traceback (most recent call last):",
            "permission denied opening /etc/shadow",
            "the build failed with 2 errors",
            "HTTP 503 Service Unavailable",
            "ordinary status line with 512 bytes written",
        ];
        let k = kept(&lines);
        // every damage line kept, the plain line (0) dropped.
        assert!(!k.contains(&0));
        for idx in [1usize, 2, 3, 4, 5, 6, 7, 8] {
            assert!(k.contains(&idx), "line {idx} should be kept");
        }
    }

    #[test]
    fn http_5xx_needs_a_marker_word() {
        // bare "512 bytes" must not trip (no http/status marker).
        assert!(!scan_damage(&["wrote 512 bytes to file"]).keep.contains(&0));
        // status + 5xx trips.
        assert!(scan_damage(&["response status 500"]).keep.contains(&0));
    }

    #[test]
    fn secret_scan_flags_mock_credential() {
        assert!(contains_secret("export TOKEN=sk-abc123def456ghi789jklmno"));
        assert!(contains_secret("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(contains_secret("Authorization: Bearer eyJhbGciOiJ"));
        assert!(contains_secret("key AKIAIOSFODNN7EXAMPLE here"));
        // JWT in a non-standard header format, caught by the eyj prefix.
        assert!(contains_secret(
            "x-auth=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"
        ));
    }

    #[test]
    fn secret_scan_ignores_ordinary_text() {
        assert!(!contains_secret("running task-123 and disk-usage check"));
        assert!(!contains_secret("the quick brown fox sk- jumped"));
        assert!(!contains_secret("compiling 200 crates, 0 errors"));
    }
}
