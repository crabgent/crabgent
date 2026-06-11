use std::collections::HashSet;

use crate::action::Action;
use crate::subject::Subject;

use super::matcher::ActionMatcher;

#[derive(Debug, Clone)]
struct AttrCondition {
    key: String,
    expected: AttrValue,
}

#[derive(Debug, Clone)]
enum AttrValue {
    Eq(String),
    OneOf(HashSet<String>),
}

impl AttrCondition {
    fn satisfied_by(&self, subject: &Subject) -> bool {
        let Some(value) = subject.attr(&self.key) else {
            return false;
        };
        match &self.expected {
            AttrValue::Eq(v) => value == v,
            AttrValue::OneOf(set) => set.contains(value),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Effect {
    Allow,
    Deny,
}

#[derive(Debug, Clone)]
pub struct Rule {
    matcher: ActionMatcher,
    conditions: Vec<AttrCondition>,
    require_scope_from_subject: bool,
    pub(super) name: Option<String>,
    pub(super) effect: Effect,
}

impl Rule {
    pub const fn allow(matcher: ActionMatcher) -> Self {
        Self {
            matcher,
            conditions: Vec::new(),
            require_scope_from_subject: false,
            name: None,
            effect: Effect::Allow,
        }
    }

    pub const fn deny(matcher: ActionMatcher) -> Self {
        Self {
            matcher,
            conditions: Vec::new(),
            require_scope_from_subject: false,
            name: None,
            effect: Effect::Deny,
        }
    }

    /// Give the rule a human-readable name for diagnostics.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Require `subject.attr(key)` to equal `value`.
    pub fn requires_attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.conditions.push(AttrCondition {
            key: key.into(),
            expected: AttrValue::Eq(value.into()),
        });
        self
    }

    /// Require `subject.attr(key)` to be a member of `values`.
    pub fn requires_attr_in<I, S>(mut self, key: impl Into<String>, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.conditions.push(AttrCondition {
            key: key.into(),
            expected: AttrValue::OneOf(values.into_iter().map(Into::into).collect()),
        });
        self
    }

    /// Require the action scope to be within the scope derived from the subject.
    pub const fn requires_scope_from_subject(mut self) -> Self {
        self.require_scope_from_subject = true;
        self
    }

    pub(super) fn matches(&self, subject: &Subject, action: &Action) -> bool {
        if !self.matcher.matches(action) {
            return false;
        }
        if self.require_scope_from_subject
            && !action
                .scope()
                .is_some_and(|scope| scope.is_within_subject(subject))
        {
            return false;
        }
        self.conditions.iter().all(|c| c.satisfied_by(subject))
    }
}
