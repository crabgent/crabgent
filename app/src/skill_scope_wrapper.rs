//! Memory-tool wrapper for downstream scope rules.
//!
//! Background: skills and tool-notes are agent-global by design - every
//! channel and every conversation an agent owns (and every human who
//! talks to it) should see every skill/tool the agent has stored. The
//! stock `MemoryTool` accepts whatever `scope` the caller passes, so a
//! skill stored from a Matrix DM (full scope: owner + channel + conv +
//! kind) is invisible to the same agent's Telegram session, because the
//! Telegram subject's auto-built scope filters the row away. We rewrite
//! both store and search/get/delete paths before delegating: any tool
//! call with `class == "skill"` or `class == "tools"` (in args or
//! scope) gets its `scope` collapsed to `{agent: <subject's agent>}`
//! only, i.e. `owner IS NULL`, the agent-global bucket.
//!
//! Other memory classes are person + agent scoped. Channel/conversation
//! fields are always removed so a memory stored in Matrix can be found
//! from Telegram or TUI by the same human.
//!
//! A caller may still explicitly request agent-global scope by passing a
//! scope with `agent` but no `owner`. This is needed for relation expansion
//! and get/delete paths after an agent-global search result. Missing scope
//! stays person + agent scoped.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{MemoryScope, Owner, Subject, Tool, ToolCtx, ToolError, ToolResult};
use serde_json::{Value, json};

use crate::agent_message::ORIGIN_OWNER_ATTR;
use crate::config::UserIdentity;
use crate::memory_scope::{MemoryScopeResolver, agent_from_subject};

pub struct SkillScopeWrapper {
    inner: Arc<dyn Tool>,
    resolver: MemoryScopeResolver,
}

impl SkillScopeWrapper {
    #[must_use]
    pub fn with_users(inner: Arc<dyn Tool>, users: &[UserIdentity]) -> Self {
        Self {
            inner,
            resolver: MemoryScopeResolver::new(users),
        }
    }

    fn normalize_args(mut args: Value, subject: &Subject, resolver: &MemoryScopeResolver) -> Value {
        let Value::Object(ref mut map) = args else {
            return args;
        };
        let class_is_global = map
            .get("class")
            .and_then(Value::as_str)
            .is_some_and(|c| c == "skill" || c == "tools");
        if !class_is_global {
            let scope = Self::normalize_person_scope(map, subject, resolver);
            map.insert("scope".to_owned(), scope);
            return args;
        }
        let agent_value = agent_from_subject(subject)
            .or_else(|| {
                map.get("scope")
                    .and_then(Value::as_object)
                    .and_then(|s| s.get("agent"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .filter(|agent| !agent.trim().is_empty());
        let scope = json!({
            "owner": Value::Null,
            "channel": Value::Null,
            "conv": Value::Null,
            "agent": agent_value.map_or(Value::Null, Value::String),
            "kind": Value::Null,
        });
        map.insert("scope".to_owned(), scope);
        args
    }

    fn normalize_person_scope(
        map: &serde_json::Map<String, Value>,
        subject: &Subject,
        resolver: &MemoryScopeResolver,
    ) -> Value {
        let scope_obj = map.get("scope").and_then(Value::as_object);
        let explicit_owner = scope_obj.and_then(|s| s.get("owner"));
        let explicit_agent = scope_obj
            .and_then(|s| s.get("agent"))
            .and_then(Value::as_str)
            .filter(|agent| !agent.trim().is_empty());
        let owner = match explicit_owner {
            Some(Value::Null) => None,
            Some(Value::String(owner)) if !owner.trim().is_empty() => {
                Some(Owner::new(resolver.canonical_owner(owner)))
            }
            None if scope_obj.is_some() && explicit_agent.is_some() => None,
            _ => {
                let owner_key = subject
                    .attr(ORIGIN_OWNER_ATTR)
                    .unwrap_or_else(|| subject.id());
                Some(Owner::new(resolver.canonical_owner(owner_key)))
            }
        };
        let agent = explicit_agent
            .map(str::to_owned)
            .or_else(|| agent_from_subject(subject));
        let mut memory_scope = MemoryScope::global();
        memory_scope.owner = owner;
        memory_scope.agent = agent;
        scope_to_value(&memory_scope)
    }
}

#[async_trait]
impl Tool for SkillScopeWrapper {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn description(&self) -> &'static str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> Value {
        self.inner.parameters_schema()
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let normalized = Self::normalize_args(args, &ctx.subject, &self.resolver);
        self.inner.execute(normalized, ctx).await
    }

    async fn execute_result(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult, ToolError> {
        let normalized = Self::normalize_args(args, &ctx.subject, &self.resolver);
        self.inner.execute_result(normalized, ctx).await
    }
}

fn scope_to_value(scope: &MemoryScope) -> Value {
    json!({
        "owner": scope
            .owner
            .as_ref()
            .map_or(Value::Null, |owner| Value::String(owner.as_str().to_owned())),
        "channel": scope
            .channel
            .as_ref()
            .map_or(Value::Null, |channel| Value::String(channel.clone())),
        "conv": scope
            .conv
            .as_ref()
            .map_or(Value::Null, |conv| Value::String(conv.clone())),
        "agent": scope
            .agent
            .as_ref()
            .map_or(Value::Null, |agent| Value::String(agent.clone())),
        "kind": scope
            .kind
            .as_ref()
            .map_or(Value::Null, |kind| Value::String(kind.clone())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn subject(agent: &str) -> Subject {
        Subject::new(format!("tui:{agent}")).with_attr("agent", agent)
    }

    fn normalize_for_agent(args: Value, agent: &str) -> Value {
        SkillScopeWrapper::normalize_args(args, &subject(agent), &MemoryScopeResolver::default())
    }

    #[test]
    fn normalize_drops_non_agent_scope_for_skill_class() {
        let args = json!({
            "op": "search",
            "class": "skill",
            "scope": {
                "owner": "matrix:@alice%3Aserver",
                "channel": "matrix",
                "conv": "matrix:!room",
                "agent": "assistant",
                "kind": "direct",
            },
            "query": "msmtp",
        });
        let normalized = normalize_for_agent(args, "assistant");
        assert_eq!(normalized["scope"]["agent"], json!("assistant"));
        assert_eq!(normalized["scope"]["owner"], Value::Null);
        assert_eq!(normalized["scope"]["channel"], Value::Null);
        assert_eq!(normalized["scope"]["conv"], Value::Null);
        assert_eq!(normalized["scope"]["kind"], Value::Null);
        assert_eq!(normalized["query"], json!("msmtp"));
    }

    #[test]
    fn normalize_drops_non_agent_scope_for_tools_class() {
        let args = json!({
            "op": "store",
            "class": "tools",
            "scope": {
                "owner": "matrix:@alice%3Aserver",
                "channel": "matrix",
                "conv": "matrix:!room",
                "agent": "assistant",
                "kind": "direct",
            },
            "body": "tool note",
        });
        let normalized = normalize_for_agent(args, "assistant");
        assert_eq!(normalized["scope"]["agent"], json!("assistant"));
        assert_eq!(normalized["scope"]["owner"], Value::Null);
        assert_eq!(normalized["scope"]["kind"], Value::Null);
    }

    #[test]
    fn normalize_uses_subject_agent_when_scope_lacks_one() {
        let args = json!({
            "op": "store",
            "class": "skill",
            "body": "skill body",
        });
        let normalized = normalize_for_agent(args, "garden");
        assert_eq!(normalized["scope"]["agent"], json!("garden"));
    }

    #[test]
    fn normalize_rewrites_non_skill_class_to_person_scope() {
        let users = vec![UserIdentity {
            canonical: "alice".to_owned(),
            owners: vec![
                "matrix:@alice%3Aserver".to_owned(),
                "telegram:42".to_owned(),
                "tui:worker".to_owned(),
            ],
        }];
        let resolver = MemoryScopeResolver::new(&users);
        let args = json!({
            "op": "search",
            "class": "notes",
            "scope": {
                "owner": "tui:worker",
                "channel": "tui",
                "conv": "tui:worker",
                "agent": "worker",
                "kind": "direct",
            },
            "query": "anything",
        });
        let normalized = SkillScopeWrapper::normalize_args(args, &subject("worker"), &resolver);
        assert_eq!(
            normalized["scope"]["owner"],
            json!("matrix:@alice%3Aserver")
        );
        assert_eq!(normalized["scope"]["agent"], json!("worker"));
        assert_eq!(normalized["scope"]["channel"], Value::Null);
        assert_eq!(normalized["scope"]["conv"], Value::Null);
        assert_eq!(normalized["scope"]["kind"], Value::Null);
    }

    #[test]
    fn normalize_rewrites_scope_when_no_class_field() {
        let args = json!({
            "op": "get",
            "doc_id": "abc",
            "scope": {
                "owner": "tui:assistant",
                "channel": "tui",
            },
        });
        let normalized = normalize_for_agent(args, "assistant");
        assert_eq!(normalized["scope"]["owner"], json!("tui:assistant"));
        assert_eq!(normalized["scope"]["agent"], json!("assistant"));
        assert_eq!(normalized["scope"]["channel"], Value::Null);
    }

    #[test]
    fn normalize_preserves_explicit_agent_only_scope_without_class() {
        let args = json!({
            "op": "relation_expand",
            "from_id": "abc",
            "scope": {
                "agent": "assistant",
            },
        });
        let normalized = normalize_for_agent(args, "assistant");
        assert_eq!(normalized["scope"]["owner"], Value::Null);
        assert_eq!(normalized["scope"]["agent"], json!("assistant"));
        assert_eq!(normalized["scope"]["channel"], Value::Null);
        assert_eq!(normalized["scope"]["conv"], Value::Null);
        assert_eq!(normalized["scope"]["kind"], Value::Null);
    }

    #[test]
    fn normalize_missing_scope_defaults_to_person_scope() {
        let args = json!({
            "op": "search",
            "query": "anything",
        });
        let normalized = normalize_for_agent(args, "assistant");
        assert_eq!(normalized["scope"]["owner"], json!("tui:assistant"));
        assert_eq!(normalized["scope"]["agent"], json!("assistant"));
    }

    #[test]
    fn normalize_preserves_explicit_null_owner_without_class() {
        let args = json!({
            "op": "get",
            "doc_id": "abc",
            "scope": {
                "owner": null,
                "agent": "assistant",
            },
        });
        let normalized = normalize_for_agent(args, "assistant");
        assert_eq!(normalized["scope"]["owner"], Value::Null);
        assert_eq!(normalized["scope"]["agent"], json!("assistant"));
    }

    #[test]
    fn normalize_handles_non_object_args_without_panic() {
        let args = json!("not an object");
        let normalized = normalize_for_agent(args.clone(), "assistant");
        assert_eq!(normalized, args);
    }
}
