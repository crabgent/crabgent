use crabgent_core::{Decision, PolicyDecision};

#[test]
fn policy_decision_matches_exhaustively_outside_crate() {
    let decision = PolicyDecision::Allow;
    let label = match decision {
        PolicyDecision::Allow => "allow",
        PolicyDecision::Deny(reason) => {
            assert!(!reason.is_empty());
            "deny"
        }
    };

    assert_eq!(label, "allow");
}

#[test]
fn hook_decision_matches_exhaustively_outside_crate() {
    let decision = Decision::Replace("updated");
    let label = match decision {
        Decision::Continue => "continue",
        Decision::Replace(value) => value,
        Decision::Deny(reason) => {
            assert!(!reason.is_empty());
            "deny"
        }
    };

    assert_eq!(label, "updated");
}
