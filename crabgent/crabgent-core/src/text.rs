use std::borrow::Cow;

/// Return the nearest UTF-8 character boundary at or before `idx`.
pub fn floor_char_boundary(s: &str, idx: usize) -> usize {
    s.floor_char_boundary(idx.min(s.len()))
}

/// Truncate a string by bytes without splitting a UTF-8 code point.
pub fn truncate_bytes_at_boundary(s: &str, max_bytes: usize) -> &str {
    s.get(..floor_char_boundary(s, max_bytes)).unwrap_or(s)
}

/// Truncate a string by Unicode scalar values.
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    s.char_indices()
        .nth(max_chars)
        .map_or(s, |(idx, _)| s.get(..idx).unwrap_or(s))
}

/// Truncate by bytes and append `suffix` when truncation happens.
///
/// When `suffix.len() >= max_bytes`, the entire suffix is emitted without a
/// head; output length may exceed `max_bytes` by at most `suffix.len() -
/// max_bytes`.
pub fn truncate_with_ellipsis<'a>(s: &'a str, max_bytes: usize, suffix: &str) -> Cow<'a, str> {
    if s.len() <= max_bytes {
        return Cow::Borrowed(s);
    }

    let head_len = floor_char_boundary(s, max_bytes.saturating_sub(suffix.len()));
    let mut out = String::with_capacity(head_len + suffix.len());
    if let Some(head) = s.get(..head_len) {
        out.push_str(head);
    }
    out.push_str(suffix);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::{
        floor_char_boundary, truncate_bytes_at_boundary, truncate_chars, truncate_with_ellipsis,
    };
    use proptest::prelude::*;

    #[test]
    fn ascii_under_cap() {
        assert_eq!(truncate_bytes_at_boundary("hello", 10), "hello");
        assert_eq!(truncate_chars("hello", 10), "hello");
    }

    #[test]
    fn ascii_over_cap() {
        assert_eq!(truncate_bytes_at_boundary("hello", 3), "hel");
        assert_eq!(truncate_chars("hello", 3), "hel");
        assert_eq!(truncate_with_ellipsis("hello", 4, "..."), "h...");
    }

    #[test]
    fn two_byte_umlaut_at_boundary() {
        assert_eq!(truncate_bytes_at_boundary("abcä", 3), "abc");
        assert_eq!(truncate_bytes_at_boundary("abcä", 4), "abc");
        assert_eq!(truncate_bytes_at_boundary("abcä", 5), "abcä");
    }

    #[test]
    fn three_byte_cjk_at_boundary() {
        assert_eq!(truncate_bytes_at_boundary("abc水", 5), "abc");
        assert_eq!(truncate_bytes_at_boundary("abc水", 6), "abc水");
    }

    #[test]
    fn four_byte_emoji_at_boundary() {
        assert_eq!(truncate_bytes_at_boundary("abc🙂", 6), "abc");
        assert_eq!(truncate_bytes_at_boundary("abc🙂", 7), "abc🙂");
    }

    #[test]
    fn zwj_emoji_sequence_truncates_to_last_code_point_boundary() {
        let family = "👨‍👩‍👧";
        let cap_inside_second_emoji = "👨‍".len() + 1;

        assert_eq!(
            truncate_bytes_at_boundary(family, cap_inside_second_emoji),
            "👨‍"
        );
    }

    #[test]
    fn empty_string() {
        assert_eq!(floor_char_boundary("", 10), 0);
        assert_eq!(truncate_bytes_at_boundary("", 10), "");
        assert_eq!(truncate_chars("", 10), "");
        assert_eq!(truncate_with_ellipsis("", 10, "..."), "");
    }

    #[test]
    fn max_zero() {
        assert_eq!(truncate_bytes_at_boundary("hello", 0), "");
        assert_eq!(truncate_chars("hello", 0), "");
        assert_eq!(truncate_with_ellipsis("hello", 0, ""), "");
    }

    #[test]
    fn max_exceeds_len() {
        assert_eq!(floor_char_boundary("hello", 99), 5);
        assert_eq!(truncate_bytes_at_boundary("hello", 99), "hello");
        assert_eq!(truncate_chars("hello", 99), "hello");
        assert_eq!(truncate_with_ellipsis("hello", 99, "..."), "hello");
    }

    #[test]
    fn ellipsis_suffix_short_string() {
        assert_eq!(truncate_with_ellipsis("abcdef", 2, "..."), "...");
    }

    proptest! {
        #[test]
        fn prop_truncate_bytes_is_char_boundary(s in ".*", n in 0..1024usize) {
            let out = truncate_bytes_at_boundary(&s, n);

            prop_assert!(s.is_char_boundary(out.len()));
            prop_assert!(out.len() <= n.min(s.len()));
            prop_assert!(s.starts_with(out));
        }

        #[test]
        fn prop_truncate_chars_count(s in ".*", n in 0..256usize) {
            let out = truncate_chars(&s, n);

            prop_assert!(out.chars().count() <= n);
            prop_assert!(s.starts_with(out));
        }

        #[test]
        fn prop_truncate_with_ellipsis_bounded(
            s in ".*",
            n in 1..1024usize,
            suffix in "[a-z]{0,16}",
        ) {
            let out = truncate_with_ellipsis(&s, n, &suffix);

            prop_assert!(out.len() <= n.max(suffix.len()) || out == s);
        }
    }
}
