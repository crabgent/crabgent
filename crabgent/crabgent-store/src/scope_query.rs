//! Backend-neutral scope predicates shared by store adapters.
//!
//! Store callers need two different interpretations of [`MemoryScope`]:
//! search/list filters treat absent fields as wildcards, while identity
//! lookups require absent fields to match stored `NULL` values. Keeping both
//! modes in one typed plan prevents each backend from inventing its own
//! nullable-column semantics.

use crabgent_core::MemoryScope;

/// Memory scope fields in the canonical backend bind/render order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeField {
    Owner,
    Channel,
    Conv,
    Agent,
    Kind,
}

/// One present scope predicate from a filter query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeFilter<'a> {
    pub field: ScopeField,
    pub value: &'a str,
}

/// Predicate value for a single scope field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeValue<'a> {
    Equals(&'a str),
    /// Owner widened to also match the agent's shared rows: renders
    /// `col IN (?, ?)` and matches when the field equals either value. Stays
    /// `Copy` because a fixed two-element array of `&str` is `Copy`.
    EqualsAny([&'a str; 2]),
    IsNull,
}

/// One scope predicate in canonical field order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopePredicate<'a> {
    pub field: ScopeField,
    pub value: ScopeValue<'a>,
}

/// Backend-neutral scope query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeQuery<'a> {
    predicates: Vec<ScopePredicate<'a>>,
}

impl<'a> ScopeQuery<'a> {
    /// Build filter semantics: `None` means wildcard.
    #[must_use]
    pub fn filter(scope: &'a MemoryScope) -> Self {
        Self {
            predicates: scope_filters(scope)
                .into_iter()
                .map(|filter| ScopePredicate {
                    field: filter.field,
                    value: ScopeValue::Equals(filter.value),
                })
                .collect(),
        }
    }

    /// Build identity semantics: `None` means stored `NULL`.
    #[must_use]
    pub fn identity(scope: &'a MemoryScope) -> Self {
        Self {
            predicates: all_scope_predicates(scope),
        }
    }

    #[must_use]
    pub fn predicates(&self) -> &[ScopePredicate<'a>] {
        &self.predicates
    }

    /// Render the scope predicates into `sql`.
    ///
    /// `push_column` writes the column name for a field; `push_placeholder`
    /// emits exactly one backend placeholder token (`?` for `SQLite`, `$N`
    /// plus index advance for Postgres). The comparison structure (`= `,
    /// `IN (.., ..)`, `IS NULL`) lives here so every backend renders
    /// `EqualsAny` as `IN (..)` through one dispatch.
    pub fn append_sql_filters<F, G>(
        &self,
        sql: &mut String,
        mut push_column: F,
        mut push_placeholder: G,
    ) where
        F: FnMut(&mut String, ScopeField),
        G: FnMut(&mut String),
    {
        for predicate in self.predicates() {
            sql.push_str(" AND ");
            push_column(sql, predicate.field);
            match predicate.value {
                ScopeValue::Equals(_) => {
                    sql.push_str(" = ");
                    push_placeholder(sql);
                }
                ScopeValue::EqualsAny(values) => {
                    sql.push_str(" IN (");
                    for (index, _) in values.iter().enumerate() {
                        if index > 0 {
                            sql.push_str(", ");
                        }
                        push_placeholder(sql);
                    }
                    sql.push(')');
                }
                ScopeValue::IsNull => sql.push_str(" IS NULL"),
            }
        }
    }

    pub fn equal_values(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.predicates()
            .iter()
            .flat_map(|predicate| match predicate.value {
                ScopeValue::Equals(value) => [Some(value), None],
                ScopeValue::EqualsAny([first, second]) => [Some(first), Some(second)],
                ScopeValue::IsNull => [None, None],
            })
            .flatten()
    }

    /// Widen the `Owner` predicate to also match the agent's shared rows.
    ///
    /// Replaces the owner `Equals` predicate with `EqualsAny([owner, agent])`
    /// so the rendered SQL becomes `owner IN (?, ?)`, binding owner then
    /// agent. No-op when there is no owner equality predicate to widen.
    pub fn widen_owner_to_shared(&mut self, agent: &'a str) {
        for predicate in &mut self.predicates {
            if predicate.field == ScopeField::Owner {
                if let ScopeValue::Equals(owner) = predicate.value {
                    predicate.value = ScopeValue::EqualsAny([owner, agent]);
                }
                return;
            }
        }
    }

    #[must_use]
    pub fn filters(&self) -> Vec<ScopeFilter<'a>> {
        self.predicates
            .iter()
            .filter_map(|predicate| match predicate.value {
                ScopeValue::Equals(value) => Some(ScopeFilter {
                    field: predicate.field,
                    value,
                }),
                // Widened owner predicates have no single-value
                // representation; filters() is single-value only and skips
                // them. Shared recall uses equal_values()/append_sql_filters.
                ScopeValue::EqualsAny(_) | ScopeValue::IsNull => None,
            })
            .collect()
    }

    #[must_use]
    pub fn matches(&self, scope: &MemoryScope) -> bool {
        self.predicates
            .iter()
            .all(|predicate| predicate_matches_scope(*predicate, scope))
    }
}

fn predicate_matches_scope(predicate: ScopePredicate<'_>, scope: &MemoryScope) -> bool {
    let actual = scope_field_value(scope, predicate.field);
    match predicate.value {
        ScopeValue::Equals(expected) => actual == Some(expected),
        ScopeValue::EqualsAny(values) => values.iter().any(|value| actual == Some(*value)),
        ScopeValue::IsNull => actual.is_none(),
    }
}

fn all_scope_predicates(scope: &MemoryScope) -> Vec<ScopePredicate<'_>> {
    [
        field_predicate(
            ScopeField::Owner,
            scope_field_value(scope, ScopeField::Owner),
        ),
        field_predicate(
            ScopeField::Channel,
            scope_field_value(scope, ScopeField::Channel),
        ),
        field_predicate(ScopeField::Conv, scope_field_value(scope, ScopeField::Conv)),
        field_predicate(
            ScopeField::Agent,
            scope_field_value(scope, ScopeField::Agent),
        ),
        field_predicate(ScopeField::Kind, scope_field_value(scope, ScopeField::Kind)),
    ]
    .into()
}

const fn field_predicate(field: ScopeField, value: Option<&str>) -> ScopePredicate<'_> {
    ScopePredicate {
        field,
        value: match value {
            Some(value) => ScopeValue::Equals(value),
            None => ScopeValue::IsNull,
        },
    }
}

fn scope_filters(scope: &MemoryScope) -> Vec<ScopeFilter<'_>> {
    let mut filters = Vec::with_capacity(5);
    for predicate in all_scope_predicates(scope) {
        if let ScopeValue::Equals(value) = predicate.value {
            filters.push(ScopeFilter {
                field: predicate.field,
                value,
            });
        }
    }
    filters
}

fn scope_field_value(scope: &MemoryScope, field: ScopeField) -> Option<&str> {
    match field {
        ScopeField::Owner => scope.owner.as_ref().map(crabgent_core::Owner::as_str),
        ScopeField::Channel => scope.channel.as_deref(),
        ScopeField::Conv => scope.conv.as_deref(),
        ScopeField::Agent => scope.agent.as_deref(),
        ScopeField::Kind => scope.kind.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use crabgent_core::Owner;

    use super::*;

    fn scope() -> MemoryScope {
        MemoryScope::for_owner(Owner::new("alice"))
            .with_channel("slack")
            .with_conv("thread")
            .with_agent("assistant")
            .with_kind("direct")
    }

    #[test]
    fn filter_keeps_backend_bind_order() {
        let scope = scope();
        let query = ScopeQuery::filter(&scope);

        let fields: Vec<_> = query
            .predicates()
            .iter()
            .map(|predicate| predicate.field)
            .collect();

        assert_eq!(
            fields,
            vec![
                ScopeField::Owner,
                ScopeField::Channel,
                ScopeField::Conv,
                ScopeField::Agent,
                ScopeField::Kind,
            ]
        );
    }

    #[test]
    fn filter_matches_only_present_fields() {
        let scoped = scope();
        let filter_scope = MemoryScope::for_owner(Owner::new("alice"))
            .with_channel("slack")
            .with_kind("direct");
        let query = ScopeQuery::filter(&filter_scope);

        assert!(query.matches(&scoped));
        assert!(!query.matches(&scoped.with_kind("group")));
    }

    #[test]
    fn identity_requires_null_for_absent_fields() {
        let identity_scope = MemoryScope::for_owner(Owner::new("alice"));
        let query = ScopeQuery::identity(&identity_scope);

        assert!(query.matches(&identity_scope));
        assert!(!query.matches(&identity_scope.clone().with_channel("slack")));
    }

    #[test]
    fn identity_global_matches_only_global_scope() {
        let global = MemoryScope::global();
        let query = ScopeQuery::identity(&global);

        assert!(query.matches(&global));
        assert!(!query.matches(&MemoryScope::for_owner(Owner::new("alice"))));
    }

    fn shared_scope() -> MemoryScope {
        MemoryScope::for_owner(Owner::new("alice")).with_agent("shared-agent")
    }

    fn render_columns(sql: &mut String, field: ScopeField) {
        let column = match field {
            ScopeField::Owner => "owner",
            ScopeField::Channel => "channel",
            ScopeField::Conv => "conv",
            ScopeField::Agent => "agent",
            ScopeField::Kind => "kind",
        };
        sql.push_str(column);
    }

    #[test]
    fn widen_owner_replaces_owner_predicate_with_equalsany() {
        let scope = shared_scope();
        let mut query = ScopeQuery::filter(&scope);
        query.widen_owner_to_shared("shared-agent");

        let owner = query
            .predicates()
            .iter()
            .find(|predicate| predicate.field == ScopeField::Owner)
            .expect("owner predicate present");
        assert_eq!(
            owner.value,
            ScopeValue::EqualsAny(["alice", "shared-agent"])
        );
    }

    #[test]
    fn widen_owner_is_noop_without_owner_predicate() {
        let scope = MemoryScope::global().with_agent("shared-agent");
        let mut query = ScopeQuery::filter(&scope);
        query.widen_owner_to_shared("shared-agent");

        assert!(
            query
                .predicates()
                .iter()
                .all(|predicate| predicate.field != ScopeField::Owner)
        );
        assert!(
            query
                .predicates()
                .iter()
                .all(|predicate| !matches!(predicate.value, ScopeValue::EqualsAny(_)))
        );
    }

    #[test]
    fn equalsany_renders_in_clause_with_two_placeholders() {
        let scope = shared_scope();
        let mut query = ScopeQuery::filter(&scope);
        query.widen_owner_to_shared("shared-agent");

        let mut sql = String::new();
        query.append_sql_filters(&mut sql, render_columns, |s| s.push('?'));

        assert!(sql.contains("owner IN (?, ?)"), "rendered: {sql}");
        assert!(sql.contains("agent = ?"), "rendered: {sql}");
    }

    #[test]
    fn exact_owner_still_renders_single_placeholder() {
        let scope = shared_scope();
        let query = ScopeQuery::filter(&scope);

        let mut sql = String::new();
        query.append_sql_filters(&mut sql, render_columns, |s| s.push('?'));

        assert!(sql.contains("owner = ?"), "rendered: {sql}");
        assert!(!sql.contains("IN ("), "rendered: {sql}");
    }

    #[test]
    fn equal_values_flattens_equalsany_in_bind_order() {
        let scope = shared_scope();
        let mut query = ScopeQuery::filter(&scope);
        query.widen_owner_to_shared("shared-agent");

        let values: Vec<_> = query.equal_values().collect();
        assert_eq!(values, vec!["alice", "shared-agent", "shared-agent"]);
    }

    #[test]
    fn equalsany_matches_either_owner_value() {
        let scope = shared_scope();
        let mut query = ScopeQuery::filter(&scope);
        query.widen_owner_to_shared("shared-agent");

        let alice_row = MemoryScope::for_owner(Owner::new("alice")).with_agent("shared-agent");
        let shared_agent_row =
            MemoryScope::for_owner(Owner::new("shared-agent")).with_agent("shared-agent");
        let bob_row = MemoryScope::for_owner(Owner::new("bob")).with_agent("shared-agent");

        assert!(query.matches(&alice_row));
        assert!(query.matches(&shared_agent_row));
        assert!(!query.matches(&bob_row));
    }
}
