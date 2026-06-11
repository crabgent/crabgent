//! `MemoryScope`: scoping vector for memory + session search.
//!
//! Subject-driven: scope mirrors the attribute set on `Subject`. All
//! fields are optional; `MemoryScope::global()` (= `::default()`) means
//! "no constraint" and is what an `AllowAllPolicy` typically passes
//! through. `MemoryScope::from_subject(&Subject)` derives a per-subject
//! scope from the standard channel attribute keys.
//!
//! Stores filter rows by AND-matching every present field. Absent
//! fields don't constrain the result.

use serde::{Deserialize, Serialize};

use crate::owner::Owner;
use crate::subject::Subject;

/// Standard subject-attribute keys consumed by `from_subject`. Kept in
/// sync with `crabgent-channel::subject::attr_keys` (intentional string
/// duplication: `crabgent-core` cannot depend on `crabgent-channel`).
mod attr_keys {
    pub const CHANNEL: &str = "channel";
    pub const CONV: &str = "conv";
    pub const AGENT: &str = "agent";
    pub const CHANNEL_KIND: &str = "channel_kind";
}

/// Scoping vector for memory + session search.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryScope {
    pub owner: Option<Owner>,
    pub channel: Option<String>,
    pub conv: Option<String>,
    pub agent: Option<String>,
    pub kind: Option<String>,
}

impl MemoryScope {
    /// "No constraint": all fields `None`. Equivalent to `::default()`.
    #[must_use]
    pub fn global() -> Self {
        Self::default()
    }

    /// Constrain to one owner, leave other fields open.
    #[must_use]
    pub fn for_owner(owner: Owner) -> Self {
        Self {
            owner: Some(owner),
            ..Self::default()
        }
    }

    /// Derive scope from a `Subject`: `subject.id()` becomes `owner`,
    /// the four standard channel attrs (`channel`, `conv`, `agent`,
    /// `channel_kind`) become the matching scope fields when present.
    #[must_use]
    pub fn from_subject(subject: &Subject) -> Self {
        Self {
            owner: Some(Owner::new(subject.id())),
            channel: subject.attr(attr_keys::CHANNEL).map(str::to_owned),
            conv: subject.attr(attr_keys::CONV).map(str::to_owned),
            agent: subject.attr(attr_keys::AGENT).map(str::to_owned),
            kind: subject.attr(attr_keys::CHANNEL_KIND).map(str::to_owned),
        }
    }

    /// `true` if this requested scope is within the scope derived from
    /// `subject`.
    #[must_use]
    pub fn is_within_subject(&self, subject: &Subject) -> bool {
        Self::from_subject(subject).matches(self)
    }

    #[must_use]
    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    #[must_use]
    pub fn with_conv(mut self, conv: impl Into<String>) -> Self {
        self.conv = Some(conv.into());
        self
    }

    #[must_use]
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    #[must_use]
    pub fn with_kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// `true` if every field present in `self` (the filter) also matches
    /// in `target`. Absent fields in `self` don't constrain.
    #[must_use]
    pub fn matches(&self, target: &Self) -> bool {
        opt_eq(self.owner.as_ref(), target.owner.as_ref())
            && opt_eq(self.channel.as_ref(), target.channel.as_ref())
            && opt_eq(self.conv.as_ref(), target.conv.as_ref())
            && opt_eq(self.agent.as_ref(), target.agent.as_ref())
            && opt_eq(self.kind.as_ref(), target.kind.as_ref())
    }
}

fn opt_eq<T: PartialEq>(filter: Option<&T>, target: Option<&T>) -> bool {
    filter.is_none_or(|f| target == Some(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_is_all_none() {
        let s = MemoryScope::global();
        assert!(s.owner.is_none());
        assert!(s.channel.is_none());
        assert!(s.conv.is_none());
        assert!(s.agent.is_none());
        assert!(s.kind.is_none());
    }

    #[test]
    fn for_owner_sets_owner_only() {
        let s = MemoryScope::for_owner(Owner::new("alice"));
        assert_eq!(s.owner, Some(Owner::new("alice")));
        assert!(s.channel.is_none());
    }

    #[test]
    fn from_subject_uses_subject_id_and_channel_attrs() {
        let subj = Subject::new("alice")
            .with_attr("channel", "slack")
            .with_attr("conv", "slack:T1/D1")
            .with_attr("agent", "alice")
            .with_attr("channel_kind", "direct");
        let s = MemoryScope::from_subject(&subj);
        assert_eq!(s.owner, Some(Owner::new("alice")));
        assert_eq!(s.channel.as_deref(), Some("slack"));
        assert_eq!(s.conv.as_deref(), Some("slack:T1/D1"));
        assert_eq!(s.agent.as_deref(), Some("alice"));
        assert_eq!(s.kind.as_deref(), Some("direct"));
    }

    #[test]
    fn from_subject_leaves_missing_attrs_none() {
        let subj = Subject::new("alice");
        let s = MemoryScope::from_subject(&subj);
        assert_eq!(s.owner, Some(Owner::new("alice")));
        assert!(s.channel.is_none());
        assert!(s.conv.is_none());
        assert!(s.agent.is_none());
        assert!(s.kind.is_none());
    }

    #[test]
    fn is_within_subject_requires_owner_match() {
        let subject = Subject::new("alice");
        assert!(MemoryScope::for_owner(Owner::new("alice")).is_within_subject(&subject));
        assert!(!MemoryScope::for_owner(Owner::new("bob")).is_within_subject(&subject));
        assert!(!MemoryScope::global().is_within_subject(&subject));
    }

    #[test]
    fn is_within_subject_requires_channel_attrs_when_present() {
        let subject = Subject::new("alice")
            .with_attr("channel", "slack")
            .with_attr("conv", "slack:T1/D1");
        let allowed = MemoryScope::for_owner(Owner::new("alice"))
            .with_channel("slack")
            .with_conv("slack:T1/D1");
        let too_broad = MemoryScope::for_owner(Owner::new("alice"));
        let wrong_conv = MemoryScope::for_owner(Owner::new("alice"))
            .with_channel("slack")
            .with_conv("slack:T1/D2");
        assert!(allowed.is_within_subject(&subject));
        assert!(!too_broad.is_within_subject(&subject));
        assert!(!wrong_conv.is_within_subject(&subject));
    }

    #[test]
    fn builder_methods_set_fields() {
        let s = MemoryScope::for_owner(Owner::new("u"))
            .with_channel("slack")
            .with_conv("c1")
            .with_agent("a1")
            .with_kind("direct");
        assert_eq!(s.channel.as_deref(), Some("slack"));
        assert_eq!(s.conv.as_deref(), Some("c1"));
        assert_eq!(s.agent.as_deref(), Some("a1"));
        assert_eq!(s.kind.as_deref(), Some("direct"));
    }

    #[test]
    fn matches_global_filter_accepts_anything() {
        let filter = MemoryScope::global();
        let target = MemoryScope::for_owner(Owner::new("u")).with_channel("slack");
        assert!(filter.matches(&target));
    }

    #[test]
    fn matches_owner_filter_rejects_other_owner() {
        let filter = MemoryScope::for_owner(Owner::new("alice"));
        let target = MemoryScope::for_owner(Owner::new("bob"));
        assert!(!filter.matches(&target));
    }

    #[test]
    fn matches_partial_filter_ignores_absent_fields() {
        let filter = MemoryScope::default().with_channel("slack");
        let target = MemoryScope::for_owner(Owner::new("u")).with_channel("slack");
        assert!(filter.matches(&target));
    }

    #[test]
    fn matches_field_mismatch_rejects() {
        let filter = MemoryScope::default().with_channel("slack");
        let target = MemoryScope::for_owner(Owner::new("u")).with_channel("telegram");
        assert!(!filter.matches(&target));
    }

    #[test]
    fn matches_target_missing_field_rejects_when_filter_set() {
        let filter = MemoryScope::default().with_channel("slack");
        let target = MemoryScope::for_owner(Owner::new("u"));
        assert!(!filter.matches(&target));
    }

    #[test]
    fn matches_all_fields_aligned() {
        let s = MemoryScope::for_owner(Owner::new("u"))
            .with_channel("slack")
            .with_conv("c")
            .with_agent("a")
            .with_kind("direct");
        assert!(s.matches(&s));
    }

    #[test]
    fn serde_round_trip_preserves_fields() {
        let s = MemoryScope::for_owner(Owner::new("u")).with_channel("slack");
        let json = serde_json::to_string(&s).expect("serialize");
        let back: MemoryScope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(s, back);
    }
}
