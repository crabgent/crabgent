//! Permission decisions: `PolicyHook` trait, `PolicyDecision`, and two
//! reference impls (`AllowAllPolicy`, `DenyAllPolicy`).

use async_trait::async_trait;

use crate::action::Action;
use crate::subject::Subject;

pub mod strict;

pub use strict::{ActionMatcher, Rule, StrictPolicy, StrictPolicyBuilder, TargetPredicate};

/// Stable verdict from a policy hook.
///
/// Future variants are a breaking API change.
#[derive(Debug, Clone)]
pub enum PolicyDecision {
    Allow,
    Deny(String),
}

/// Permission policy hook.
///
/// The kernel calls `allow` before each LLM call and each tool dispatch.
/// A policy is required at build time (the `KernelBuilder` is typestate
/// and refuses to `build()` without one set).
#[async_trait]
pub trait PolicyHook: Send + Sync {
    async fn allow(&self, subject: &Subject, action: &Action) -> PolicyDecision;
}

/// Allows everything. Wild-west / homelab default. Set explicitly on
/// the `KernelBuilder`; the kernel never picks this for you.
pub struct AllowAllPolicy;

#[async_trait]
impl PolicyHook for AllowAllPolicy {
    async fn allow(&self, _subject: &Subject, _action: &Action) -> PolicyDecision {
        PolicyDecision::Allow
    }
}

/// Denies everything. Useful for tests, fail-closed setups, or as a
/// starting point that consumers wrap with their own conditional logic.
pub struct DenyAllPolicy;

#[async_trait]
impl PolicyHook for DenyAllPolicy {
    async fn allow(&self, _subject: &Subject, _action: &Action) -> PolicyDecision {
        PolicyDecision::Deny("denied by DenyAllPolicy".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allow_all_returns_allow() {
        let p = AllowAllPolicy;
        let s = Subject::new("u");
        let r = p.allow(&s, &Action::LlmCall).await;
        assert!(matches!(r, PolicyDecision::Allow));
    }

    #[tokio::test]
    async fn allow_all_for_tool_call() {
        let p = AllowAllPolicy;
        let s = Subject::new("u").with_attr("role", "admin");
        let r = p.allow(&s, &Action::tool("bash")).await;
        assert!(matches!(r, PolicyDecision::Allow));
    }

    #[tokio::test]
    async fn deny_all_blocks_with_reason() {
        let p = DenyAllPolicy;
        let s = Subject::new("u");
        let r = p.allow(&s, &Action::tool("bash")).await;
        match r {
            PolicyDecision::Deny(reason) => assert!(reason.contains("DenyAllPolicy")),
            PolicyDecision::Allow => panic!("expected deny"),
        }
    }

    #[tokio::test]
    async fn deny_all_blocks_llm_call_too() {
        let p = DenyAllPolicy;
        let s = Subject::new("u");
        let r = p.allow(&s, &Action::LlmCall).await;
        assert!(matches!(r, PolicyDecision::Deny(_)));
    }

    #[test]
    fn deny_carries_reason_string() {
        let d = PolicyDecision::Deny("rate limited".into());
        match d {
            PolicyDecision::Deny(reason) => assert_eq!(reason, "rate limited"),
            PolicyDecision::Allow => panic!("wrong variant"),
        }
    }
}

#[cfg(test)]
mod strict_action_family_tests;
#[cfg(test)]
mod strict_consolidation_tests;
#[cfg(test)]
mod strict_relation_tests;
#[cfg(test)]
mod strict_tests;
