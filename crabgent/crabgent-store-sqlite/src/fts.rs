//! Shared helpers for `SQLite` FTS5 query construction.

/// Tokenize user input into a safe FTS5 query string.
///
/// Splits on ASCII whitespace and wraps every token in FTS5 phrase
/// quotes (escaping embedded `"` as `""`). The resulting tokens are
/// joined with a space, which FTS5 interprets as an implicit `AND`.
/// This means multi-word queries match documents that contain every
/// token (in any order, anywhere in the body), aligned with the
/// `websearch_to_tsquery` default on the Postgres backend and with the
/// way users naturally type free-text searches.
///
/// Per-token phrase quoting also escapes any FTS5 operator characters
/// (`*`, `:`, `+`, `-`, `^`, `AND`, `OR`, `NOT`, `NEAR`, parens) inside
/// a token, so untrusted text is searched as content rather than parsed
/// as an operator expression. Phrase search ("red wine" as a contiguous
/// match) is not currently exposed through this helper; callers that
/// need it should build a dedicated FTS5 fragment.
///
/// Stemming divergence vs. Postgres: FTS5 with the `unicode61` tokenizer
/// (the default for our `memory_fts` / `session_messages_fts` tables)
/// does not stem tokens, so searching `runs` will not match a body
/// containing `running`. The Postgres backend applies language-specific
/// stemming through `websearch_to_tsquery`, so recall can be wider
/// there. This is a known backend difference, not a bug in this helper.
#[must_use]
pub fn quote_fts_phrase(query: &str) -> String {
    query
        .split_whitespace()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_token_wraps_in_phrase_quotes() {
        assert_eq!(quote_fts_phrase("hello"), "\"hello\"");
    }

    #[test]
    fn multi_word_query_becomes_implicit_and() {
        assert_eq!(quote_fts_phrase("hello world"), "\"hello\" \"world\"");
    }

    #[test]
    fn embedded_quotes_in_token_are_escaped() {
        assert_eq!(quote_fts_phrase("say \"hi\""), "\"say\" \"\"\"hi\"\"\"");
    }

    #[test]
    fn empty_query_collapses_to_empty_string() {
        assert_eq!(quote_fts_phrase(""), "");
    }

    #[test]
    fn collapses_runs_of_whitespace() {
        assert_eq!(quote_fts_phrase("  red   wine  "), "\"red\" \"wine\"");
    }

    #[test]
    fn fts5_operator_tokens_are_quoted_literally() {
        // Operator words become literal phrase searches so untrusted
        // input cannot break out of the AND-of-tokens contract.
        assert_eq!(quote_fts_phrase("a OR b"), "\"a\" \"OR\" \"b\"");
    }
}
