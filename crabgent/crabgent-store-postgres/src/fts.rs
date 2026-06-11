//! Full-text search helpers for Postgres.
//!
//! GENERATED tsvector columns hardcode `'german'` (see the
//! `20260518000001_switch_fts_to_german` migration). The query side pins the
//! same language explicitly via the 2-arg form
//! `websearch_to_tsquery('german', $1)` / `ts_headline('german', ...)`, so the
//! query stemmer matches the index regardless of a connection's
//! `default_text_search_config`. The migration still sets that default via
//! `ALTER DATABASE` as a belt-and-suspenders baseline for ad-hoc queries.

/// Normalize user search text before binding it into `websearch_to_tsquery`.
#[must_use]
pub fn normalize_websearch_query(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize_websearch_query(" alpha \n beta "), "alpha beta");
    }
}
