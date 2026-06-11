//! Shared interpretation of [`SearchQuery`] for memory backends.
//!
//! Backends own storage-specific rendering. This module owns the semantic
//! decisions that must stay identical across in-memory, `SQLite`, and Postgres:
//! scope field order, lifecycle filters, time bounds, limit/offset conversion,
//! vector candidate count, and ranking intent.

use chrono::{DateTime, Utc};
use crabgent_core::{MemoryScope, OwnerMatch, SearchQuery};
use crabgent_log::warn;

use crate::records::MemoryDoc;
use crate::scope_query::ScopeQuery;

/// Ranking strategy selected from the query shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RankingIntent {
    ImportanceThenCreatedAt,
    TextRelevanceThenImportanceThenCreatedAt,
    HybridScoreThenImportanceThenCreatedAt,
}

/// Backend-neutral search plan derived from a [`SearchQuery`].
#[derive(Debug, Clone)]
pub struct MemorySearchPlan<'a> {
    pub text: Option<&'a str>,
    pub embedding: Option<&'a [f32]>,
    pub scope: ScopeQuery<'a>,
    pub class: Option<&'a str>,
    pub expires_after: Option<DateTime<Utc>>,
    pub filter_archived: bool,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub limit_i64: i64,
    pub offset_i64: i64,
    pub limit_usize: usize,
    pub offset_usize: usize,
    pub vector_k: Option<i64>,
    pub ranking: RankingIntent,
}

impl<'a> MemorySearchPlan<'a> {
    #[must_use]
    pub fn new(query: &'a SearchQuery) -> Self {
        let text = (!query.query.is_empty()).then_some(query.query.as_str());
        let embedding = query.embedding.as_deref();
        let ranking = match (text, embedding) {
            (_, Some(_)) => RankingIntent::HybridScoreThenImportanceThenCreatedAt,
            (Some(_), None) => RankingIntent::TextRelevanceThenImportanceThenCreatedAt,
            (None, None) => RankingIntent::ImportanceThenCreatedAt,
        };
        let scope = shared_scope(query);
        let limit_i64 = i64::from(query.limit);
        let offset_i64 = i64::from(query.offset);
        Self {
            text,
            embedding,
            scope,
            class: query.class.as_deref(),
            expires_after: (!query.include_expired).then(Utc::now),
            filter_archived: !query.include_archived,
            since: query.since,
            until: query.until,
            limit_i64,
            offset_i64,
            limit_usize: usize::try_from(query.limit).unwrap_or(usize::MAX),
            offset_usize: usize::try_from(query.offset).unwrap_or(usize::MAX),
            vector_k: embedding.map(|_| (limit_i64 + offset_i64).clamp(1, 4096)),
            ranking,
        }
    }

    #[must_use]
    pub fn matches_doc(&self, doc: &MemoryDoc) -> bool {
        self.matches_scope(&doc.scope)
            && self
                .class
                .is_none_or(|class| doc.class.as_deref() == Some(class))
            && self.expires_after.is_none_or(|expires_after| {
                doc.expires_at
                    .is_none_or(|expires_at| expires_at > expires_after)
            })
            && (!self.filter_archived || doc.archived_at.is_none())
            && self.since.is_none_or(|since| doc.updated_at >= since)
            && self.until.is_none_or(|until| doc.updated_at <= until)
    }

    #[must_use]
    pub fn matches_scope(&self, scope: &MemoryScope) -> bool {
        self.scope.matches(scope)
    }
}

/// Build the scope filter, widening the owner predicate for shared recall.
///
/// `OwnerMatch::IncludingShared` widens `owner = ?` to `owner IN (owner,
/// agent)` so the caller's private rows and the agent's shared rows both
/// match. Widening needs both an owner and an agent: with an owner but no
/// agent it is a misconfiguration (nothing to widen to), so it logs and
/// degrades to owner-only. Without an owner there is no owner predicate to
/// widen, so it is a silent no-op.
fn shared_scope(query: &SearchQuery) -> ScopeQuery<'_> {
    let mut scope = ScopeQuery::filter(&query.scope);
    if query.owner_match != OwnerMatch::IncludingShared {
        return scope;
    }
    let Some(owner) = query.scope.owner.as_ref() else {
        return scope;
    };
    let Some(agent) = query.scope.agent.as_deref() else {
        warn!(
            owner = %owner,
            "shared memory recall requested without scope.agent; degrading to owner-only"
        );
        return scope;
    };
    scope.widen_owner_to_shared(agent);
    scope
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use crabgent_core::Owner;

    use super::*;
    use crate::scope_query::{ScopeField, ScopeQuery};

    fn scope() -> MemoryScope {
        MemoryScope::for_owner(Owner::new("alice"))
            .with_channel("slack")
            .with_conv("thread")
            .with_agent("assistant")
            .with_kind("direct")
    }

    #[test]
    fn scope_filters_keep_backend_bind_order() {
        let query = SearchQuery::new("x").scope(scope());
        let plan = MemorySearchPlan::new(&query);

        let fields: Vec<_> = plan
            .scope
            .predicates()
            .iter()
            .map(|filter| filter.field)
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
    fn scope_plan_matches_only_present_fields() {
        let scoped = scope();
        let filter_scope = MemoryScope::for_owner(Owner::new("alice"))
            .with_channel("slack")
            .with_kind("direct");
        let plan = ScopeQuery::filter(&filter_scope);

        assert!(plan.matches(&scoped));
        assert!(!plan.matches(&scoped.with_kind("group")));
    }

    #[test]
    fn plan_selects_lifecycle_and_ranking() {
        let query = SearchQuery::new("hello")
            .scope(scope())
            .class("semantic")
            .embedding(vec![0.1, 0.2])
            .limit(20)
            .offset(7);
        let plan = MemorySearchPlan::new(&query);

        assert_eq!(plan.text, Some("hello"));
        assert_eq!(
            plan.ranking,
            RankingIntent::HybridScoreThenImportanceThenCreatedAt
        );
        assert_eq!(plan.class, Some("semantic"));
        assert!(plan.expires_after.is_some());
        assert!(plan.filter_archived);
        assert_eq!(plan.vector_k, Some(27));
    }

    use crate::scope_query::ScopeValue;

    fn owner_predicate_value<'a>(plan: &MemorySearchPlan<'a>) -> ScopeValue<'a> {
        plan.scope
            .predicates()
            .iter()
            .find(|predicate| predicate.field == ScopeField::Owner)
            .map(|predicate| predicate.value)
            .expect("owner predicate present")
    }

    #[test]
    fn include_shared_widens_owner_predicate_when_owner_and_agent_present() {
        let scope = MemoryScope::for_owner(Owner::new("alice")).with_agent("shared-agent");
        let query = SearchQuery::new("x").scope(scope).include_shared(true);
        let plan = MemorySearchPlan::new(&query);

        assert_eq!(
            owner_predicate_value(&plan),
            ScopeValue::EqualsAny(["alice", "shared-agent"])
        );
    }

    #[test]
    fn include_shared_is_noop_without_owner() {
        let scope = MemoryScope::global().with_agent("shared-agent");
        let query = SearchQuery::new("x").scope(scope).include_shared(true);
        let plan = MemorySearchPlan::new(&query);

        assert!(
            plan.scope
                .predicates()
                .iter()
                .all(|predicate| predicate.field != ScopeField::Owner)
        );
    }

    #[test]
    fn include_shared_without_agent_degrades_to_owner_only() {
        let scope = MemoryScope::for_owner(Owner::new("alice"));
        let query = SearchQuery::new("x").scope(scope).include_shared(true);
        let plan = MemorySearchPlan::new(&query);

        assert_eq!(
            owner_predicate_value(&plan),
            ScopeValue::Equals("alice"),
            "owner-only Exact behavior when no agent to widen to"
        );
    }

    #[test]
    fn exact_owner_match_keeps_single_owner_predicate() {
        let scope = MemoryScope::for_owner(Owner::new("alice")).with_agent("shared-agent");
        let query = SearchQuery::new("x").scope(scope);
        let plan = MemorySearchPlan::new(&query);

        assert_eq!(owner_predicate_value(&plan), ScopeValue::Equals("alice"));
    }

    #[test]
    fn matches_doc_applies_lifecycle_and_time_filters() {
        let now = Utc::now();
        let query = SearchQuery::new("")
            .scope(MemoryScope::for_owner(Owner::new("alice")))
            .since(now - Duration::hours(2))
            .until(now + Duration::hours(2));
        let plan = MemorySearchPlan::new(&query);
        let mut doc = MemoryDoc::new(MemoryScope::for_owner(Owner::new("alice")), "remember this");
        doc.updated_at = now;

        assert!(plan.matches_doc(&doc));

        doc.archived_at = Some(now);
        assert!(!plan.matches_doc(&doc));
    }
}
