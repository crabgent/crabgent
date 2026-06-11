use crate::owner::Owner;

/// Predicate for matching an action target owner.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TargetPredicate {
    Any,
    Exact(Owner),
    Prefix(String),
}

impl TargetPredicate {
    pub(super) fn matches(&self, owner: &Owner) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(target) => owner == target,
            Self::Prefix(prefix) => owner.as_str().starts_with(prefix),
        }
    }
}

/// Match an expected qualifier against the action's qualifier.
///
/// Wildcard semantics: `expected=None` matches any actual qualifier.
/// Strict mode: `expected=Some(name)` matches only `actual=Some(same name)`;
/// `expected=Some(_)` and `actual=None` is no-match.
pub fn qualifier_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    match expected {
        Some(expected) => matches!(actual, Some(actual) if actual == expected),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::qualifier_matches;

    #[test]
    fn expected_none_actual_none_matches() {
        assert!(qualifier_matches(None, None));
    }

    #[test]
    fn expected_none_actual_some_matches() {
        assert!(qualifier_matches(None, Some("stub")));
    }

    #[test]
    fn expected_some_actual_none_no_match() {
        assert!(!qualifier_matches(Some("stub"), None));
    }

    #[test]
    fn expected_some_actual_some_eq_matches() {
        assert!(qualifier_matches(Some("stub"), Some("stub")));
    }

    #[test]
    fn expected_some_actual_some_neq_no_match() {
        assert!(!qualifier_matches(Some("stub"), Some("other")));
    }
}
