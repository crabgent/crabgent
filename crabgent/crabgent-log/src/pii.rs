//! PII redaction wrappers for crabgent-log.
//!
//! `RedactedUid` and `RedactedText` are the always-on path for subjects,
//! message content, and tool arguments. Debug builds support `CRABGENT_LOG_PII`
//! as a local troubleshooting bypass. Release builds compile the bypass out.

use std::fmt;

/// Display wrapper for logging subject identifiers without exposing raw PII.
///
/// The redacted form is a deterministic FNV-1a u64 hash rendered as
/// `u:` followed by 16 lowercase hex chars.
#[derive(Clone, Copy)]
pub struct RedactedUid<'a>(pub(crate) &'a str);

/// Display wrapper for logging free-form text without exposing its contents.
///
/// The redacted form preserves only byte length as `[REDACTED len=N]`.
#[derive(Clone, Copy)]
pub struct RedactedText<'a>(pub(crate) &'a str);

/// Redact a subject or user identifier for logging.
pub const fn redact_uid(uid: &str) -> RedactedUid<'_> {
    RedactedUid(uid)
}

/// Redact message content, prompt text, response text, or tool arguments.
pub const fn redact_text(text: &str) -> RedactedText<'_> {
    RedactedText(text)
}

#[cfg(debug_assertions)]
fn parse_pii_bypass(value: Option<&str>) -> bool {
    let Some(trimmed) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return false;
    };

    trimmed != "0" && !trimmed.eq_ignore_ascii_case("false") && !trimmed.eq_ignore_ascii_case("no")
}

#[cfg(debug_assertions)]
static PII_BYPASS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

#[cfg(test)]
thread_local! {
    static PII_BYPASS_TEST_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(debug_assertions)]
#[expect(
    clippy::redundant_pub_crate,
    reason = "parent module calls this private-module helper"
)]
pub(crate) fn pii_bypass_enabled() -> bool {
    #[cfg(test)]
    {
        if let Some(value) = PII_BYPASS_TEST_OVERRIDE.with(std::cell::Cell::get) {
            return value;
        }
    }

    *PII_BYPASS.get_or_init(|| parse_pii_bypass(std::env::var("CRABGENT_LOG_PII").ok().as_deref()))
}

#[cfg(not(debug_assertions))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "parent module calls this private-module helper"
)]
pub(crate) const fn pii_bypass_enabled() -> bool {
    false
}

impl fmt::Display for RedactedUid<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if pii_bypass_enabled() {
            return f.write_str(self.0);
        }

        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.0.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        write!(f, "u:{hash:016x}")
    }
}

impl fmt::Debug for RedactedUid<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for RedactedText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if pii_bypass_enabled() {
            return f.write_str(self.0);
        }

        write!(f, "[REDACTED len={}]", self.0.len())
    }
}

impl fmt::Debug for RedactedText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::{PII_BYPASS_TEST_OVERRIDE, parse_pii_bypass, redact_text, redact_uid};

    #[cfg(debug_assertions)]
    #[test]
    fn parse_pii_bypass_false_values() {
        assert!(!parse_pii_bypass(None));
        assert!(!parse_pii_bypass(Some("")));
        assert!(!parse_pii_bypass(Some("   ")));
        assert!(!parse_pii_bypass(Some("0")));
        assert!(!parse_pii_bypass(Some("false")));
        assert!(!parse_pii_bypass(Some("FALSE")));
        assert!(!parse_pii_bypass(Some("no")));
        assert!(!parse_pii_bypass(Some("NO")));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn parse_pii_bypass_true_values() {
        assert!(parse_pii_bypass(Some("1")));
        assert!(parse_pii_bypass(Some("true")));
        assert!(parse_pii_bypass(Some("TRUE")));
        assert!(parse_pii_bypass(Some("yes")));
        assert!(parse_pii_bypass(Some("on")));
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_bypass_renders_raw_when_override_true() {
        PII_BYPASS_TEST_OVERRIDE.with(|cell| cell.set(Some(true)));
        let uid = format!("{}", redact_uid("subject-123"));
        let text = format!("{}", redact_text("tool args"));
        PII_BYPASS_TEST_OVERRIDE.with(|cell| cell.set(None));

        assert_eq!(uid, "subject-123");
        assert_eq!(text, "tool args");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_bypass_still_redacts_when_override_false() {
        PII_BYPASS_TEST_OVERRIDE.with(|cell| cell.set(Some(false)));
        let uid = format!("{}", redact_uid("subject-123"));
        let text = format!("{}", redact_text("tool args"));
        PII_BYPASS_TEST_OVERRIDE.with(|cell| cell.set(None));

        assert!(uid.starts_with("u:"));
        assert!(!uid.contains("subject-123"));
        assert_eq!(text, "[REDACTED len=9]");
    }
}
