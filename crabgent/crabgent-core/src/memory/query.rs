//! `SearchQuery`: builder for memory + session search.
//!
//! Bundles the user query, scope filter, time bounds, limit, and
//! offset. Defaults: `limit = 10`, `offset = 0`, scope = global,
//! `since`/`until` unset (= no time filter), embedding unset, lifecycle
//! filters enabled.

use chrono::{DateTime, Utc};

use super::scope::MemoryScope;

/// Default search `limit` when the caller does not set one.
pub const DEFAULT_SEARCH_LIMIT: u32 = 10;
/// Maximum search `limit` accepted by shared memory/session query surfaces.
pub const MAX_SEARCH_LIMIT: u32 = 100;

/// Owner-matching mode for a memory search.
///
/// A memory row is PRIVATE when its `owner` is a user id, and SHARED when its
/// `owner` is the agent's id. A shared row is visible to every user of that
/// agent: skills, tool-notes, and deliberately shared semantic knowledge are
/// stored this way by the caller (store with `owner` set to the agent id).
/// The write path never marks a row shared on its own; shared storage is a
/// caller convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OwnerMatch {
    /// Match only the scope owner. Returns the caller's private rows.
    #[default]
    Exact,
    /// Also match the agent's shared rows: the caller's private rows plus rows
    /// owned by the agent in `scope.agent`. Never returns another user's
    /// private rows.
    IncludingShared,
}

/// Search input bundle. Construct via `SearchQuery::new(query)` and
/// chain the optional filters.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    pub query: String,
    pub scope: MemoryScope,
    pub class: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub include_expired: bool,
    pub include_archived: bool,
    pub owner_match: OwnerMatch,
    pub limit: u32,
    pub offset: u32,
}

impl SearchQuery {
    /// Construct a query with default limit and global scope.
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            scope: MemoryScope::default(),
            class: None,
            embedding: None,
            since: None,
            until: None,
            include_expired: false,
            include_archived: false,
            owner_match: OwnerMatch::Exact,
            limit: DEFAULT_SEARCH_LIMIT,
            offset: 0,
        }
    }

    #[must_use]
    pub fn scope(mut self, scope: MemoryScope) -> Self {
        self.scope = scope;
        self
    }

    #[must_use]
    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.class = Some(class.into());
        self
    }

    #[must_use]
    pub fn embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }

    #[must_use]
    pub const fn since(mut self, t: DateTime<Utc>) -> Self {
        self.since = Some(t);
        self
    }

    #[must_use]
    pub const fn until(mut self, t: DateTime<Utc>) -> Self {
        self.until = Some(t);
        self
    }

    #[must_use]
    pub const fn include_expired(mut self) -> Self {
        self.include_expired = true;
        self
    }

    #[must_use]
    pub const fn include_archived(mut self) -> Self {
        self.include_archived = true;
        self
    }

    /// Set the owner-matching mode. `Exact` (default) returns only the scope
    /// owner's rows; `IncludingShared` also returns the agent's shared rows.
    #[must_use]
    pub const fn owner_match(mut self, mode: OwnerMatch) -> Self {
        self.owner_match = mode;
        self
    }

    /// Toggle shared-row recall. `true` sets `OwnerMatch::IncludingShared`,
    /// `false` resets to `OwnerMatch::Exact`.
    #[must_use]
    pub const fn include_shared(mut self, shared: bool) -> Self {
        self.owner_match = if shared {
            OwnerMatch::IncludingShared
        } else {
            OwnerMatch::Exact
        };
        self
    }

    #[must_use]
    pub const fn limit(mut self, n: u32) -> Self {
        self.limit = if n > MAX_SEARCH_LIMIT {
            MAX_SEARCH_LIMIT
        } else {
            n
        };
        self
    }

    #[must_use]
    pub const fn offset(mut self, n: u32) -> Self {
        self.offset = n;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owner::Owner;
    use chrono::TimeZone;

    #[test]
    fn new_sets_default_limit_and_global_scope() {
        let q = SearchQuery::new("hello");
        assert_eq!(q.query, "hello");
        assert_eq!(q.limit, DEFAULT_SEARCH_LIMIT);
        assert_eq!(q.offset, 0);
        assert_eq!(q.scope, MemoryScope::global());
        assert!(q.class.is_none());
        assert!(q.embedding.is_none());
        assert!(q.since.is_none());
        assert!(q.until.is_none());
        assert!(!q.include_expired);
        assert!(!q.include_archived);
        assert_eq!(q.owner_match, OwnerMatch::Exact);
    }

    #[test]
    fn owner_match_defaults_to_exact() {
        assert_eq!(OwnerMatch::default(), OwnerMatch::Exact);
    }

    #[test]
    fn include_shared_true_sets_including_shared() {
        let q = SearchQuery::new("x").include_shared(true);
        assert_eq!(q.owner_match, OwnerMatch::IncludingShared);
    }

    #[test]
    fn include_shared_false_resets_to_exact() {
        let q = SearchQuery::new("x")
            .owner_match(OwnerMatch::IncludingShared)
            .include_shared(false);
        assert_eq!(q.owner_match, OwnerMatch::Exact);
    }

    #[test]
    fn owner_match_setter_overrides_default() {
        let q = SearchQuery::new("x").owner_match(OwnerMatch::IncludingShared);
        assert_eq!(q.owner_match, OwnerMatch::IncludingShared);
    }

    #[test]
    fn scope_replaces_default_scope() {
        let s = MemoryScope::for_owner(Owner::new("u"));
        let q = SearchQuery::new("x").scope(s.clone());
        assert_eq!(q.scope, s);
    }

    #[test]
    fn class_setter() {
        let q = SearchQuery::new("x").class("episodic");
        assert_eq!(q.class.as_deref(), Some("episodic"));
    }

    #[test]
    fn embedding_default_none() {
        assert!(SearchQuery::new("x").embedding.is_none());
    }

    #[test]
    fn embedding_setter() {
        let q = SearchQuery::new("x").embedding(vec![0.25, 0.5, 1.0]);
        assert_eq!(q.embedding, Some(vec![0.25, 0.5, 1.0]));
    }

    #[test]
    fn since_and_until_set_time_bounds() {
        let t1 = Utc
            .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
            .single()
            .expect("valid test datetime");
        let t2 = Utc
            .with_ymd_and_hms(2026, 2, 1, 0, 0, 0)
            .single()
            .expect("valid test datetime");
        let q = SearchQuery::new("x").since(t1).until(t2);
        assert_eq!(q.since, Some(t1));
        assert_eq!(q.until, Some(t2));
    }

    #[test]
    fn include_expired_default_false() {
        assert!(!SearchQuery::new("x").include_expired);
    }

    #[test]
    fn include_expired_sets_flag() {
        assert!(SearchQuery::new("x").include_expired().include_expired);
    }

    #[test]
    fn include_archived_default_false() {
        assert!(!SearchQuery::new("x").include_archived);
    }

    #[test]
    fn include_archived_sets_flag() {
        assert!(SearchQuery::new("x").include_archived().include_archived);
    }

    #[test]
    fn limit_overrides_default() {
        let q = SearchQuery::new("x").limit(50);
        assert_eq!(q.limit, 50);
    }

    #[test]
    fn limit_clamps_to_max() {
        let q = SearchQuery::new("x").limit(MAX_SEARCH_LIMIT + 1);
        assert_eq!(q.limit, MAX_SEARCH_LIMIT);
    }

    #[test]
    fn offset_paginates() {
        let q = SearchQuery::new("x").offset(20);
        assert_eq!(q.offset, 20);
    }

    #[test]
    fn builder_chain_is_associative() {
        let s = MemoryScope::for_owner(Owner::new("u")).with_channel("slack");
        let q = SearchQuery::new("hello")
            .scope(s.clone())
            .class("semantic")
            .embedding(vec![0.1, 0.2])
            .include_expired()
            .include_archived()
            .limit(5)
            .offset(10);
        assert_eq!(q.query, "hello");
        assert_eq!(q.scope, s);
        assert_eq!(q.class.as_deref(), Some("semantic"));
        assert_eq!(q.embedding, Some(vec![0.1, 0.2]));
        assert!(q.include_expired);
        assert!(q.include_archived);
        assert_eq!(q.limit, 5);
        assert_eq!(q.offset, 10);
    }

    #[test]
    fn limit_zero_is_legal_value() {
        let q = SearchQuery::new("x").limit(0);
        assert_eq!(q.limit, 0);
    }

    #[test]
    fn query_accepts_owned_string() {
        let owned = String::from("foo bar");
        let q = SearchQuery::new(owned.clone());
        assert_eq!(q.query, owned);
    }
}
