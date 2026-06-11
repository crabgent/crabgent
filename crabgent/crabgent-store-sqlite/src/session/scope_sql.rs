//! Scope predicate SQL helpers for the `SQLite` session store.

use crabgent_store::scope_query::{ScopeField, ScopeQuery};

pub(super) fn append_search_filters(sql: &mut String, query: &ScopeQuery<'_>, prefix: &str) {
    append_filters(sql, query, prefix, search_column);
}

pub(super) fn append_identity_filters(sql: &mut String, query: &ScopeQuery<'_>) {
    append_filters(sql, query, "", identity_column);
}

fn append_filters(
    sql: &mut String,
    query: &ScopeQuery<'_>,
    prefix: &str,
    column_for: fn(ScopeField) -> &'static str,
) {
    query.append_sql_filters(
        sql,
        |sql, field| {
            sql.push_str(prefix);
            sql.push_str(column_for(field));
        },
        |sql| sql.push('?'),
    );
}

const fn search_column(field: ScopeField) -> &'static str {
    match field {
        ScopeField::Owner => "owner",
        ScopeField::Channel => "channel",
        ScopeField::Conv => "conv",
        ScopeField::Agent => "agent",
        ScopeField::Kind => "kind",
    }
}

const fn identity_column(field: ScopeField) -> &'static str {
    match field {
        ScopeField::Owner => "owner",
        other => prefixed_column(other),
    }
}

const fn prefixed_column(field: ScopeField) -> &'static str {
    match field {
        ScopeField::Channel => "scope_channel",
        ScopeField::Conv => "scope_conv",
        ScopeField::Agent => "scope_agent",
        ScopeField::Kind => "scope_kind",
        ScopeField::Owner => "owner",
    }
}
