//! Secret redaction for Anthropic API error bodies.
//!
//! Error bodies returned by the Messages API may echo request fragments
//! that contain the configured API key or other secret-like tokens. These
//! helpers strip such material before the body is surfaced in a
//! `ProviderError::Api` message. All functions are private to the crate;
//! `client::api_error_message` is the only caller.

/// Replace the configured API key (exact match) and any secret-like spans
/// in `body` with `[REDACTED]`.
pub fn redact_error_body(body: &str, api_key: &str) -> String {
    let body = if api_key.is_empty() {
        body.to_owned()
    } else {
        body.replace(api_key, "[REDACTED]")
    };
    redact_secret_like_spans(&body)
}

fn redact_secret_like_spans(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        let rest = input
            .get(cursor..)
            // invariant: cursor only advances by whole-char byte lengths
            // (see the two branches below), so it stays on a char boundary.
            .expect("cursor advances by char boundary");
        let span_len = if starts_with_ignore_ascii_case(rest, "sk-")
            || starts_with_ignore_ascii_case(rest, "secret")
        {
            Some(secret_token_span_len(rest))
        } else if starts_with_ignore_ascii_case(rest, "api_key") {
            Some(api_key_span_len(rest))
        } else {
            None
        };

        if let Some(span_len) = span_len {
            output.push_str("[REDACTED]");
            cursor += span_len;
        } else {
            // invariant: cursor < input.len() (loop guard) and on a char
            // boundary, so at least one char remains.
            let ch = rest.chars().next().expect("cursor is in bounds");
            output.push(ch);
            cursor += ch.len_utf8();
        }
    }
    output
}

fn starts_with_ignore_ascii_case(input: &str, needle: &str) -> bool {
    input
        .get(..needle.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(needle))
}

fn secret_token_span_len(input: &str) -> usize {
    input
        .char_indices()
        .take_while(|(_, ch)| is_secret_token_char(*ch))
        .last()
        .map_or_else(
            || input.chars().next().map_or(0, char::len_utf8),
            |(idx, ch)| idx + ch.len_utf8(),
        )
}

const fn is_secret_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | '=')
}

fn api_key_span_len(input: &str) -> usize {
    const KEY_LEN: usize = "api_key".len();
    let Some(after_key) = input.get(KEY_LEN..) else {
        return KEY_LEN;
    };
    let separator_probe = KEY_LEN + delimiter_prefix_len(after_key);
    let Some(after_delimiters) = input.get(separator_probe..) else {
        return KEY_LEN;
    };
    let Some(separator) = after_delimiters.chars().next() else {
        return KEY_LEN;
    };
    if !matches!(separator, ':' | '=') {
        return KEY_LEN;
    }

    let value_start = separator_probe
        + separator.len_utf8()
        + delimiter_prefix_len(
            after_delimiters
                .get(separator.len_utf8()..)
                // invariant: `separator` is the first char of
                // `after_delimiters`, so slicing past its byte length lands
                // on a char boundary.
                .expect("separator length advances by char boundary"),
        );
    let Some(value) = input.get(value_start..) else {
        return input.len();
    };
    let value_len = value
        .char_indices()
        .take_while(|(_, ch)| is_api_key_value_char(*ch))
        .last()
        .map_or(0, |(idx, ch)| idx + ch.len_utf8());
    (value_start + value_len).max(KEY_LEN)
}

fn delimiter_prefix_len(input: &str) -> usize {
    input
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_whitespace() || matches!(*ch, '"' | '\''))
        .last()
        .map_or(0, |(idx, ch)| idx + ch.len_utf8())
}

const fn is_api_key_value_char(ch: char) -> bool {
    if ch.is_ascii_whitespace() {
        false
    } else {
        !matches!(ch, '"' | '\'' | ',' | '}' | ']')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_message_redacts_secret_like_tokens() {
        let redacted = redact_error_body(
            r#"max_tokens failed for {"message":"token=sk-ant-api03-leaked","api_key":"secret-test-ant-key-99999"}"#,
            "sk-ant-api03-x",
        );

        assert!(redacted.contains("max_tokens"));
        assert!(redacted.contains("[REDACTED]"));
        assert!(!redacted.contains("secret-test"));
        assert!(!redacted.contains("sk-ant-api03-leaked"));
    }
}
