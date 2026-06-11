//! Pure-string unit tests for the memory search SQL builder. These need no
//! database, so they run in any environment (no Postgres container).

use crabgent_core::{MemoryScope, Owner, SearchQuery};

use super::build_search_sql;

#[test]
fn search_sql_pins_german_text_search_config() {
    // Regression: the 1-arg websearch_to_tsquery reads default_text_search_config,
    // which a connection override could break. The generated search_vector column
    // hardcodes 'german', so the query side must pin the same language.
    let query = SearchQuery::new("hallo welt").scope(MemoryScope::for_owner(Owner::new("alice")));
    let (sql, _bindings) = build_search_sql(&query);

    assert!(
        sql.contains("websearch_to_tsquery('german', $1)"),
        "FTS query must pin german, got: {sql}"
    );
    assert!(
        !sql.contains("websearch_to_tsquery($1)"),
        "no bare 1-arg form may remain, got: {sql}"
    );
}

#[test]
fn search_sql_captures_expires_boundary_at_build_time() {
    // Regression: MemorySearchPlan::new calls Utc::now() for expires_after, so
    // the plan/bindings must be built once. Default include_expired = false, so
    // the expiry clause and its bound value are present and stable here, not
    // recomputed per retry inside the caller's retry loop.
    let query = SearchQuery::new("x").scope(MemoryScope::for_owner(Owner::new("alice")));
    let (sql, bindings) = build_search_sql(&query);

    assert!(
        sql.contains("expires_at IS NULL OR expires_at >"),
        "default search filters expired rows, got: {sql}"
    );
    assert!(
        bindings.plan.expires_after.is_some(),
        "expires_after is captured into the plan at build time"
    );
}

#[test]
fn search_sql_omits_expires_boundary_when_include_expired() {
    let query = SearchQuery::new("x")
        .scope(MemoryScope::for_owner(Owner::new("alice")))
        .include_expired();
    let (sql, bindings) = build_search_sql(&query);

    assert!(
        !sql.contains("expires_at >"),
        "include_expired drops the expiry bound, got: {sql}"
    );
    assert!(bindings.plan.expires_after.is_none());
}

#[test]
fn search_sql_is_deterministic_for_same_query() {
    let query = SearchQuery::new("repeat").scope(MemoryScope::for_owner(Owner::new("alice")));
    let (sql_a, _) = build_search_sql(&query);
    let (sql_b, _) = build_search_sql(&query);
    assert_eq!(sql_a, sql_b, "the builder is pure: same query, same SQL");
}
