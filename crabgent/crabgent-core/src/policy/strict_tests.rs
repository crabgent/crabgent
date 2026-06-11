//! Test suite for [`super::StrictPolicy`].

use super::strict::{ActionMatcher, Rule, StrictPolicy};
use super::{PolicyDecision, PolicyHook};
use crate::{Action, MemoryId, MemoryScope, Owner, Subject};

fn user(role: &str) -> Subject {
    Subject::new("u").with_attr("role", role)
}

#[tokio::test]
async fn default_denies_when_no_rule_matches() {
    let p = StrictPolicy::builder().build();
    let r = p.allow(&Subject::new("u"), &Action::LlmCall).await;
    assert!(matches!(r, PolicyDecision::Deny(s) if s.contains("no matching allow rule")));
}

#[tokio::test]
async fn allow_llm_call_unconditional() {
    let p = StrictPolicy::builder().allow_llm_call().build();
    let r = p.allow(&Subject::new("u"), &Action::LlmCall).await;
    assert!(matches!(r, PolicyDecision::Allow));
}

#[tokio::test]
async fn allow_tool_exact_match() {
    let p = StrictPolicy::builder().allow_tool("read_file").build();
    let r = p
        .allow(&Subject::new("u"), &Action::tool("read_file"))
        .await;
    assert!(matches!(r, PolicyDecision::Allow));
}

#[tokio::test]
async fn allow_tool_does_not_leak_to_other_tools() {
    let p = StrictPolicy::builder().allow_tool("read_file").build();
    let r = p.allow(&Subject::new("u"), &Action::tool("bash")).await;
    assert!(matches!(r, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn allow_tool_for_requires_attr() {
    let p = StrictPolicy::builder()
        .allow_tool_for("write_file", "role", "writer")
        .build();
    let writer = user("writer");
    let viewer = user("viewer");
    let r1 = p.allow(&writer, &Action::tool("write_file")).await;
    let r2 = p.allow(&viewer, &Action::tool("write_file")).await;
    assert!(matches!(r1, PolicyDecision::Allow));
    assert!(matches!(r2, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn deny_short_circuits_before_default_allow() {
    let p = StrictPolicy::builder()
        .deny_tool("bash")
        .allow_by_default()
        .build();
    let r = p.allow(&Subject::new("u"), &Action::tool("bash")).await;
    assert!(matches!(r, PolicyDecision::Deny(s) if s.contains("denied by rule")));
}

#[tokio::test]
async fn first_match_wins_in_order() {
    let p = StrictPolicy::builder()
        .allow_tool("bash")
        .deny_tool("bash")
        .build();
    let r = p.allow(&Subject::new("u"), &Action::tool("bash")).await;
    assert!(matches!(r, PolicyDecision::Allow));
}

#[tokio::test]
async fn requires_attr_in_set_matches_any_member() {
    let p = StrictPolicy::builder()
        .rule(
            Rule::allow(ActionMatcher::Tool("update_file".into()))
                .requires_attr_in("role", ["writer", "editor", "admin"]),
        )
        .build();
    let editor = user("editor");
    let admin = user("admin");
    let viewer = user("viewer");
    for s in [&editor, &admin] {
        let r = p.allow(s, &Action::tool("update_file")).await;
        assert!(matches!(r, PolicyDecision::Allow));
    }
    let r = p.allow(&viewer, &Action::tool("update_file")).await;
    assert!(matches!(r, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn missing_attr_blocks_conditional_rule() {
    let p = StrictPolicy::builder()
        .allow_tool_for("write_file", "role", "writer")
        .build();
    let no_attr = Subject::new("u");
    let r = p.allow(&no_attr, &Action::tool("write_file")).await;
    assert!(matches!(r, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn allow_custom_action() {
    let p = StrictPolicy::builder().allow_custom("memory.read").build();
    let r = p
        .allow(&Subject::new("u"), &Action::custom("memory.read"))
        .await;
    assert!(matches!(r, PolicyDecision::Allow));
}

#[tokio::test]
async fn any_matcher_catches_all_action_kinds() {
    let p = StrictPolicy::builder()
        .rule(Rule::allow(ActionMatcher::Any))
        .build();
    let s = Subject::new("u");
    for a in [Action::LlmCall, Action::tool("x"), Action::custom("y")] {
        let r = p.allow(&s, &a).await;
        assert!(matches!(r, PolicyDecision::Allow));
    }
}

#[tokio::test]
async fn allow_by_default_permits_unmatched() {
    let p = StrictPolicy::builder().allow_by_default().build();
    let r = p.allow(&Subject::new("u"), &Action::tool("anything")).await;
    assert!(matches!(r, PolicyDecision::Allow));
}

#[tokio::test]
async fn deny_by_default_is_explicit_alias() {
    let p = StrictPolicy::builder()
        .allow_by_default()
        .deny_by_default()
        .build();
    let r = p.allow(&Subject::new("u"), &Action::LlmCall).await;
    assert!(matches!(r, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn multiple_attr_conditions_combine_with_and() {
    let p = StrictPolicy::builder()
        .rule(
            Rule::allow(ActionMatcher::Tool("write_file".into()))
                .requires_attr("role", "writer")
                .requires_attr("team", "core"),
        )
        .build();
    let core_writer = Subject::new("u")
        .with_attr("role", "writer")
        .with_attr("team", "core");
    let other_writer = Subject::new("u")
        .with_attr("role", "writer")
        .with_attr("team", "other");
    let r1 = p.allow(&core_writer, &Action::tool("write_file")).await;
    let r2 = p.allow(&other_writer, &Action::tool("write_file")).await;
    assert!(matches!(r1, PolicyDecision::Allow));
    assert!(matches!(r2, PolicyDecision::Deny(_)));
}

#[tokio::test]
async fn deny_action_carries_action_name_in_reason() {
    let p = StrictPolicy::builder().deny_tool("bash").build();
    let r = p.allow(&Subject::new("u"), &Action::tool("bash")).await;
    match r {
        PolicyDecision::Deny(reason) => assert!(reason.contains("bash")),
        PolicyDecision::Allow => panic!("expected deny, got allow"),
    }
}

#[tokio::test]
async fn deny_reason_can_include_rule_name() {
    let p = StrictPolicy::builder()
        .rule(Rule::deny(ActionMatcher::Tool("bash".into())).name("block_bash"))
        .build();
    let r = p.allow(&Subject::new("u"), &Action::tool("bash")).await;
    match r {
        PolicyDecision::Deny(reason) => {
            assert!(reason.contains("block_bash"));
            assert!(reason.contains("index 1"));
        }
        PolicyDecision::Allow => panic!("expected deny, got allow"),
    }
}

fn memory_search_action() -> Action {
    Action::MemorySearch {
        query: "x".into(),
        scope: MemoryScope::for_owner(Owner::new("u")),
    }
}

fn memory_store_action() -> Action {
    Action::MemoryStore {
        scope: MemoryScope::for_owner(Owner::new("u")),
    }
}

fn memory_get_action() -> Action {
    Action::MemoryGet {
        id: MemoryId::new(),
        scope: MemoryScope::for_owner(Owner::new("u")),
    }
}

fn memory_delete_action() -> Action {
    Action::MemoryDelete {
        id: MemoryId::new(),
        scope: MemoryScope::for_owner(Owner::new("u")),
    }
}

fn memory_archive_action() -> Action {
    Action::MemoryArchive {
        id: MemoryId::new(),
        scope: MemoryScope::for_owner(Owner::new("u")),
    }
}

fn memory_unarchive_action() -> Action {
    Action::MemoryUnarchive {
        id: MemoryId::new(),
        scope: MemoryScope::for_owner(Owner::new("u")),
    }
}

fn memory_extend_expiry_action() -> Action {
    Action::MemoryExtendExpiry {
        id: MemoryId::new(),
        scope: MemoryScope::for_owner(Owner::new("u")),
    }
}

fn session_search_action() -> Action {
    Action::SessionSearch {
        query: "x".into(),
        scope: MemoryScope::for_owner(Owner::new("u")),
    }
}

#[tokio::test]
async fn allow_memory_search_only_lets_search_through() {
    let p = StrictPolicy::builder().allow_memory_search().build();
    let s = Subject::new("u");
    assert!(matches!(
        p.allow(&s, &memory_search_action()).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        p.allow(&s, &memory_store_action()).await,
        PolicyDecision::Deny(_)
    ));
    assert!(matches!(
        p.allow(&s, &memory_delete_action()).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn allow_memory_any_covers_all_memory_variants() {
    let p = StrictPolicy::builder().allow_memory_any().build();
    let s = Subject::new("u");
    for action in [
        memory_search_action(),
        memory_store_action(),
        memory_get_action(),
        memory_delete_action(),
        memory_archive_action(),
        memory_unarchive_action(),
        memory_extend_expiry_action(),
    ] {
        assert!(matches!(p.allow(&s, &action).await, PolicyDecision::Allow));
    }
}

#[tokio::test]
async fn memory_any_then_deny_specific_first_match_wins() {
    let p = StrictPolicy::builder()
        .allow_memory_any()
        .rule(Rule::deny(ActionMatcher::MemoryStore).requires_scope_from_subject())
        .build();
    let s = Subject::new("u");

    assert!(matches!(
        p.allow(&s, &memory_store_action()).await,
        PolicyDecision::Allow
    ));
}

#[tokio::test]
async fn deny_specific_then_allow_memory_any_first_match_wins() {
    let p = StrictPolicy::builder()
        .rule(Rule::deny(ActionMatcher::MemoryStore).requires_scope_from_subject())
        .allow_memory_any()
        .build();
    let s = Subject::new("u");

    assert!(matches!(
        p.allow(&s, &memory_store_action()).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn allow_memory_delete_can_be_attribute_gated() {
    let p = StrictPolicy::builder()
        .rule(Rule::allow(ActionMatcher::MemoryDelete).requires_attr("role", "admin"))
        .build();
    let admin = Subject::new("a").with_attr("role", "admin");
    let viewer = Subject::new("v").with_attr("role", "viewer");
    assert!(matches!(
        p.allow(&admin, &memory_delete_action()).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        p.allow(&viewer, &memory_delete_action()).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn allow_session_search_distinct_from_memory_search() {
    let p = StrictPolicy::builder().allow_session_search().build();
    let s = Subject::new("u");
    assert!(matches!(
        p.allow(&s, &session_search_action()).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        p.allow(&s, &memory_search_action()).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn any_matcher_covers_new_variants_too() {
    let p = StrictPolicy::builder()
        .rule(Rule::allow(ActionMatcher::Any))
        .build();
    let s = Subject::new("u");
    for a in [
        memory_search_action(),
        memory_store_action(),
        memory_get_action(),
        memory_delete_action(),
        memory_archive_action(),
        memory_unarchive_action(),
        memory_extend_expiry_action(),
        session_search_action(),
    ] {
        assert!(matches!(p.allow(&s, &a).await, PolicyDecision::Allow));
    }
}

#[tokio::test]
async fn memory_scope_must_match_subject_owner() {
    let p = StrictPolicy::builder().allow_memory_any().build();
    let alice = Subject::new("alice");
    let own_scope = MemoryScope::for_owner(Owner::new("alice"));
    let bob_scope = MemoryScope::for_owner(Owner::new("bob"));
    let allowed = Action::MemorySearch {
        query: "x".into(),
        scope: own_scope,
    };
    let cross_owner = Action::MemorySearch {
        query: "x".into(),
        scope: bob_scope,
    };
    let global = Action::MemorySearch {
        query: "x".into(),
        scope: MemoryScope::global(),
    };
    assert!(matches!(
        p.allow(&alice, &allowed).await,
        PolicyDecision::Allow
    ));
    assert!(matches!(
        p.allow(&alice, &cross_owner).await,
        PolicyDecision::Deny(_)
    ));
    assert!(matches!(
        p.allow(&alice, &global).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn session_scope_must_match_subject_owner() {
    let p = StrictPolicy::builder().allow_session_search().build();
    let alice = Subject::new("alice");
    let action = Action::SessionSearch {
        query: "x".into(),
        scope: MemoryScope::for_owner(Owner::new("bob")),
    };
    assert!(matches!(
        p.allow(&alice, &action).await,
        PolicyDecision::Deny(_)
    ));
}

#[tokio::test]
async fn audio_retain_denied_by_default() {
    let p = StrictPolicy::builder().build();
    let action = Action::AudioRetain {
        scope: MemoryScope::for_owner(Owner::new("alice")),
    };
    let PolicyDecision::Deny(reason) = p.allow(&Subject::new("alice"), &action).await else {
        panic!("retention must be fail-closed when no rule allows it");
    };
    assert!(reason.contains("audio.retain"));
    assert!(!reason.contains("alice"));
}

#[tokio::test]
async fn audio_retain_allowed_for_own_scope() {
    let p = StrictPolicy::builder().allow_audio_retain().build();
    let alice = Subject::new("alice");
    let own = Action::AudioRetain {
        scope: MemoryScope::for_owner(Owner::new("alice")),
    };
    assert!(matches!(p.allow(&alice, &own).await, PolicyDecision::Allow));
}

#[tokio::test]
async fn audio_retain_cross_owner_denied() {
    let p = StrictPolicy::builder().allow_audio_retain().build();
    let alice = Subject::new("alice");
    let cross = Action::AudioRetain {
        scope: MemoryScope::for_owner(Owner::new("bob")),
    };
    assert!(matches!(
        p.allow(&alice, &cross).await,
        PolicyDecision::Deny(_)
    ));
}
