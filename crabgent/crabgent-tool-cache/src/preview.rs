//! Byte-bounded, char-boundary-safe preview and slicing helpers.
//!
//! Preview helpers operate on byte indices but never split a UTF-8
//! codepoint.

use crabgent_core::text::truncate_with_ellipsis;

const ELLIPSIS: &str = "...";

/// Build a short preview of `content`. If `content` is at most
/// `max_bytes` long, returns it verbatim; otherwise truncates to the
/// nearest char boundary and appends "...".
///
/// `max_bytes` zero produces just "...". The returned string is always
/// valid UTF-8.
#[must_use]
pub fn smart_preview(content: &str, max_bytes: usize) -> String {
    if content.len() <= max_bytes {
        return content.to_owned();
    }
    truncate_with_ellipsis(content, max_bytes + ELLIPSIS.len(), ELLIPSIS).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_core::text::floor_char_boundary;

    #[test]
    fn short_content_returned_verbatim() {
        let s = "hello";
        assert_eq!(smart_preview(s, 100), "hello");
    }

    #[test]
    fn long_content_truncated_with_ellipsis() {
        let s = "abcdefghij";
        let p = smart_preview(s, 4);
        assert_eq!(p, "abcd...");
    }

    #[test]
    fn truncation_respects_char_boundary() {
        let s = "abc\u{1F600}def";
        let p = smart_preview(s, 4);
        assert_eq!(p, "abc...");
    }

    #[test]
    fn floor_boundary_at_existing_boundary_is_noop() {
        let s = "hello";
        assert_eq!(floor_char_boundary(s, 3), 3);
    }

    #[test]
    fn floor_boundary_walks_back_for_multibyte() {
        let s = "a\u{1F600}b";
        assert_eq!(floor_char_boundary(s, 3), 1);
        assert_eq!(floor_char_boundary(s, 5), 5);
    }

    #[test]
    fn floor_boundary_clamps_to_len() {
        let s = "abc";
        assert_eq!(floor_char_boundary(s, 999), 3);
    }

    #[test]
    fn empty_content_with_zero_max_returns_empty() {
        assert_eq!(smart_preview("", 0), "");
    }

    #[test]
    fn equal_length_content_returned_verbatim() {
        let s = "abcd";
        assert_eq!(smart_preview(s, 4), "abcd");
    }
}
