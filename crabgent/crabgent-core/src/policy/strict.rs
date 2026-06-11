//! # Strict policy
//!
//! Reference [`PolicyHook`] implementation with allow-by-attribute
//! semantics. Configure via the [`StrictPolicy::builder`] chain:
//!
//! ```ignore
//! let policy = StrictPolicy::builder()
//!     .allow_llm_call()
//!     .allow_tool("read_file")
//!     .allow_tool_for("write_file", "role", "writer")
//!     .deny_tool("bash")
//!     .deny_by_default()
//!     .build();
//! ```
//!
//! Evaluation order: rules iterate in registration order, first match
//! wins (Allow or Deny). If no rule matches, the default (allow vs.
//! deny) decides. The default is `Deny` unless [`allow_by_default`] is
//! called explicitly. Pair this with the typestate `KernelBuilder` to
//! stop a permissive policy from being used by accident.
//!
//! [`PolicyHook`]: crate::PolicyHook
//! [`allow_by_default`]: StrictPolicyBuilder::allow_by_default

use async_trait::async_trait;

use crate::action::Action;
use crate::policy::{PolicyDecision, PolicyHook};
use crate::subject::Subject;

mod builder;
mod matcher;
mod rule;
mod target_match;

pub use builder::StrictPolicyBuilder;
pub use matcher::ActionMatcher;
use rule::Effect;
pub use rule::Rule;
pub use target_match::TargetPredicate;

pub struct StrictPolicy {
    pub(super) rules: Vec<Rule>,
    pub(super) default_allow: bool,
}

impl StrictPolicy {
    pub const fn builder() -> StrictPolicyBuilder {
        StrictPolicyBuilder {
            rules: Vec::new(),
            default_allow: false,
        }
    }

    fn evaluate(&self, subject: &Subject, action: &Action) -> PolicyDecision {
        for (idx, rule) in self.rules.iter().enumerate() {
            if rule.matches(subject, action) {
                return match rule.effect {
                    Effect::Allow => PolicyDecision::Allow,
                    Effect::Deny => PolicyDecision::Deny(deny_rule_reason(
                        rule.name.as_deref(),
                        idx + 1,
                        action,
                    )),
                };
            }
        }
        if self.default_allow {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny(default_deny_reason(action))
        }
    }
}

#[async_trait]
impl PolicyHook for StrictPolicy {
    async fn allow(&self, subject: &Subject, action: &Action) -> PolicyDecision {
        self.evaluate(subject, action)
    }
}

fn deny_rule_reason(rule_name: Option<&str>, rule_index: usize, action: &Action) -> String {
    let rule_tag = rule_name.map_or_else(
        || format!("rule at index {rule_index}"),
        |name| format!("rule '{name}' (index {rule_index})"),
    );
    format!("denied by {rule_tag} for {}", action.name())
}

fn default_deny_reason(action: &Action) -> String {
    format!("no matching allow rule for {}", action.name())
}
