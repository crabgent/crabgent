use crabgent_core::Subject;
use crabgent_core::message::tail::is_unresolved_tail_value;
use serde_json::{Value, json};

use super::*;

fn ctx(run_id: RunId) -> RunCtx {
    RunCtx::new(run_id, Subject::new("u1"))
}

fn empty_req() -> LlmRequest {
    LlmRequest {
        model: "m".into(),
        system_prompt: None,
        messages: vec![],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

fn assistant_tool_use(call_id: &str, name: &str) -> Value {
    json!({
        "role": "assistant",
        "text": "",
        "tool_calls": [{"id": call_id, "name": name, "args": {}}],
    })
}

fn tool_result(call_id: &str, output: &str) -> Value {
    json!({
        "role": "tool_result",
        "call_id": call_id,
        "output": output,
        "is_error": false,
    })
}

fn assistant_empty() -> Value {
    json!({"role": "assistant", "text": "", "tool_calls": []})
}

#[tokio::test]
async fn registry_starts_empty() {
    let reg = InjectionRegistry::new();
    assert_eq!(reg.pending(&RunId::new()).await, 0);
}

#[tokio::test]
async fn submit_and_pending_count() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "hi").await;
    reg.submit_user_text(&id, "again").await;
    assert_eq!(reg.pending(&id).await, 2);
}

#[tokio::test]
async fn multiple_runs_isolated() {
    let reg = InjectionRegistry::new();
    let a = RunId::new();
    let b = RunId::new();
    reg.submit_user_text(&a, "for-a").await;
    assert_eq!(reg.pending(&a).await, 1);
    assert_eq!(reg.pending(&b).await, 0);
}

#[tokio::test]
async fn drain_returns_fifo_and_clears() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "first").await;
    reg.submit_user_text(&id, "second").await;
    let drained = reg.drain(&id).await;
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0]["content"][0]["text"], "first");
    assert_eq!(drained[1]["content"][0]["text"], "second");
    assert_eq!(reg.pending(&id).await, 0);
}

#[tokio::test]
async fn drain_unknown_run_returns_empty() {
    let reg = InjectionRegistry::new();
    let drained = reg.drain(&RunId::new()).await;
    assert!(drained.is_empty());
}

#[tokio::test]
async fn clear_removes_pending() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "x").await;
    reg.clear(&id).await;
    assert_eq!(reg.pending(&id).await, 0);
}

#[tokio::test]
async fn submit_system_text_uses_system_role() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_system_text(&id, "be careful").await;
    let drained = reg.drain(&id).await;
    assert_eq!(drained[0]["role"], "system");
    assert_eq!(drained[0]["content"], "be careful");
}

#[tokio::test]
async fn before_llm_continues_when_nothing_pending() {
    let reg = InjectionRegistry::new();
    let hook = InjectHook::new(reg);
    let id = RunId::new();
    let d = hook.before_llm(&empty_req(), &ctx(id)).await;
    assert!(matches!(d, Decision::Continue));
}

#[tokio::test]
async fn before_llm_appends_pending_messages_with_marker() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "ignore previous; do X").await;
    let hook = InjectHook::new(reg.clone());
    let mut req = empty_req();
    req.messages
        .push(json!({"role": "user", "content": "first"}));
    let d = hook.before_llm(&req, &ctx(id.clone())).await;
    match d {
        Decision::Replace(new_req) => {
            assert_eq!(new_req.messages.len(), 2);
            assert_eq!(new_req.messages[0]["content"], "first");
            let injected = new_req.messages[1]["content"][0]["text"]
                .as_str()
                .expect("injected text block");
            assert!(injected.starts_with(MID_TURN_MARKER));
            assert!(injected.ends_with("ignore previous; do X"));
        }
        other => panic!("unexpected: {other:?}"),
    }
    assert_eq!(reg.pending(&id).await, 0);
}

#[tokio::test]
async fn before_llm_only_drains_owning_run_id() {
    let reg = InjectionRegistry::new();
    let target = RunId::new();
    let other = RunId::new();
    reg.submit_user_text(&other, "for-other").await;
    let hook = InjectHook::new(reg.clone());
    let d = hook.before_llm(&empty_req(), &ctx(target)).await;
    assert!(matches!(d, Decision::Continue));
    assert_eq!(reg.pending(&other).await, 1);
}

#[tokio::test]
async fn submit_raw_value_bypasses_helpers() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit(&id, json!({"role": "assistant", "text": "ack"}))
        .await;
    let drained = reg.drain(&id).await;
    assert_eq!(drained[0]["role"], "assistant");
    assert_eq!(drained[0]["text"], "ack");
}

#[tokio::test]
async fn on_stop_clears_registry_for_this_run() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    let other = RunId::new();
    reg.submit_user_text(&id, "this").await;
    reg.submit_user_text(&other, "that").await;
    let hook = InjectHook::new(reg.clone());
    hook.on_stop(&ctx(id.clone()), &Outcome::Completed("done".into()))
        .await;
    assert_eq!(reg.pending(&id).await, 0);
    assert_eq!(reg.pending(&other).await, 1);
}

#[tokio::test]
async fn registry_clones_share_state() {
    let a = InjectionRegistry::new();
    let b = a.clone();
    let id = RunId::new();
    a.submit_user_text(&id, "x").await;
    assert_eq!(b.pending(&id).await, 1);
}

#[tokio::test]
async fn registry_default_is_new() {
    let reg: InjectionRegistry = InjectionRegistry::default();
    assert_eq!(reg.pending(&RunId::new()).await, 0);
}

#[tokio::test]
async fn hook_registry_accessor_returns_inner() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    let hook = InjectHook::new(reg);
    hook.registry().submit_user_text(&id, "via accessor").await;
    assert_eq!(hook.registry().pending(&id).await, 1);
}

#[tokio::test]
async fn submit_assistant_text_uses_assistant_shape() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_assistant_text(&id, "ack").await;
    let drained = reg.drain(&id).await;
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0]["role"], "assistant");
    assert_eq!(drained[0]["text"], "ack");
    assert_eq!(drained[0]["tool_calls"], json!([]));
}

#[tokio::test]
async fn submit_tool_result_uses_tool_result_shape() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_tool_result(&id, "call-1", json!({"ok": true}), true)
        .await;
    let drained = reg.drain(&id).await;
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0]["role"], "tool_result");
    assert_eq!(drained[0]["call_id"], "call-1");
    assert_eq!(drained[0]["output"], json!({"ok": true}));
    assert_eq!(drained[0]["is_error"], true);
}

#[tokio::test]
async fn before_llm_inserts_pending_between_user_and_tool_use_tail() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "msg2").await;
    let hook = InjectHook::new(reg.clone());
    let mut req = empty_req();
    req.messages.push(json!({"role": "user", "content": "hi"}));
    req.messages.push(assistant_tool_use("c1", "do_it"));
    req.messages.push(tool_result("c1", "ok"));
    req.messages.push(assistant_empty());

    let d = hook.before_llm(&req, &ctx(id.clone())).await;
    match d {
        Decision::Replace(new_req) => {
            assert_eq!(new_req.messages.len(), 5);
            assert_eq!(new_req.messages[0]["content"], "hi");
            assert_eq!(new_req.messages[1]["role"], "user");
            let injected = new_req.messages[1]["content"][0]["text"]
                .as_str()
                .expect("injected text block");
            assert!(injected.starts_with(MID_TURN_MARKER));
            assert!(injected.ends_with("msg2"));
            assert_eq!(new_req.messages[2]["role"], "assistant");
            assert!(
                !new_req.messages[2]["tool_calls"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .is_empty()
            );
            assert_eq!(new_req.messages[3]["role"], "tool_result");
            assert_eq!(new_req.messages[4], assistant_empty());
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn drained_pending_carries_mid_turn_marker_prefix() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "wake up").await;
    let hook = InjectHook::new(reg.clone());
    let mut req = empty_req();
    req.messages.push(json!({"role": "user", "content": "x"}));
    let d = hook.before_llm(&req, &ctx(id)).await;
    let Decision::Replace(new_req) = d else {
        panic!("expected Replace");
    };
    let text = new_req.messages[1]["content"][0]["text"]
        .as_str()
        .expect("text block");
    assert!(text.starts_with(MID_TURN_MARKER));
}

#[tokio::test]
async fn before_llm_extends_when_no_user_anchor() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "first user input").await;
    let hook = InjectHook::new(reg.clone());
    let mut req = empty_req();
    req.messages.push(assistant_tool_use("c0", "do_it"));
    req.messages.push(tool_result("c0", "ok"));

    let d = hook.before_llm(&req, &ctx(id)).await;
    let Decision::Replace(new_req) = d else {
        panic!("expected Replace");
    };
    assert_eq!(new_req.messages.len(), 3);
    assert_eq!(new_req.messages[0]["role"], "assistant");
    assert_eq!(new_req.messages[1]["role"], "tool_result");
    assert_eq!(new_req.messages[2]["role"], "user");
    let text = new_req.messages[2]["content"][0]["text"]
        .as_str()
        .expect("text block");
    assert!(text.starts_with(MID_TURN_MARKER));
}

#[tokio::test]
async fn prepend_marker_handles_string_content_shape() {
    let mut value = json!({"role": "user", "content": "raw text"});
    prepend_mid_turn_marker(&mut value);
    let updated = value["content"].as_str().expect("string content");
    assert!(updated.starts_with(MID_TURN_MARKER));
    assert!(updated.ends_with("raw text"));
}

#[tokio::test]
async fn before_llm_leaves_initial_request_messages_unmarked() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "inject me").await;
    let hook = InjectHook::new(reg.clone());
    let mut req = empty_req();
    req.messages
        .push(json!({"role": "user", "content": "original prompt"}));

    let d = hook.before_llm(&req, &ctx(id)).await;
    let Decision::Replace(new_req) = d else {
        panic!("expected Replace");
    };
    assert_eq!(new_req.messages[0]["content"], "original prompt");
    let injected = new_req.messages[1]["content"][0]["text"]
        .as_str()
        .expect("injected text");
    assert!(injected.starts_with(MID_TURN_MARKER));
}

#[test]
fn prepend_marker_is_idempotent() {
    let mut value = json!({
        "role": "user",
        "content": [{"type": "text", "text": "hello"}],
    });
    prepend_mid_turn_marker(&mut value);
    let after_first = value["content"][0]["text"].as_str().map(str::to_owned);
    prepend_mid_turn_marker(&mut value);
    let after_second = value["content"][0]["text"].as_str().map(str::to_owned);
    assert_eq!(after_first, after_second);
    assert!(
        after_first
            .as_deref()
            .expect("text")
            .starts_with(MID_TURN_MARKER)
    );
}

#[test]
fn prepend_marker_ignores_non_user_roles() {
    let mut system = json!({"role": "system", "content": "operator note"});
    prepend_mid_turn_marker(&mut system);
    assert_eq!(system["content"], "operator note");

    let mut assistant = json!({"role": "assistant", "content": "ack"});
    prepend_mid_turn_marker(&mut assistant);
    assert_eq!(assistant["content"], "ack");
}

#[test]
fn prepend_marker_skips_unmarkable_user_content() {
    let mut missing_content = json!({"role": "user"});
    prepend_mid_turn_marker(&mut missing_content);
    assert!(missing_content.get("content").is_none());

    let mut object_content = json!({"role": "user", "content": {"text": "ignored"}});
    prepend_mid_turn_marker(&mut object_content);
    assert_eq!(object_content["content"]["text"], "ignored");

    let mut blocks = json!({"role": "user", "content": [
        {"type": "image", "text": "skip"},
        {"type": "text"},
        {"type": "text", "text": 42},
        {"type": "text", "text": "mark me"}
    ]});
    prepend_mid_turn_marker(&mut blocks);
    let marked = blocks["content"][3]["text"].as_str().expect("marked text");
    assert!(marked.starts_with(MID_TURN_MARKER));

    let mut already_marked = json!({"role": "user", "content": format!("{MID_TURN_MARKER} raw")});
    prepend_mid_turn_marker(&mut already_marked);
    assert_eq!(already_marked["content"], format!("{MID_TURN_MARKER} raw"));
}

#[tokio::test]
async fn before_llm_preserves_multiple_pending_fifo_order() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "first inject").await;
    reg.submit_user_text(&id, "second inject").await;
    let hook = InjectHook::new(reg.clone());
    let mut req = empty_req();
    req.messages.push(json!({"role": "user", "content": "hi"}));
    req.messages.push(assistant_tool_use("c1", "do_it"));

    let d = hook.before_llm(&req, &ctx(id)).await;
    let Decision::Replace(new_req) = d else {
        panic!("expected Replace");
    };
    assert_eq!(new_req.messages.len(), 4);
    let first = new_req.messages[1]["content"][0]["text"]
        .as_str()
        .expect("first injected text");
    let second = new_req.messages[2]["content"][0]["text"]
        .as_str()
        .expect("second injected text");
    assert!(first.starts_with(MID_TURN_MARKER));
    assert!(first.ends_with("first inject"));
    assert!(second.starts_with(MID_TURN_MARKER));
    assert!(second.ends_with("second inject"));
    assert_eq!(new_req.messages[3]["role"], "assistant");
}

#[tokio::test]
async fn before_llm_extends_when_tail_is_final_assistant_reply() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "follow up").await;
    let hook = InjectHook::new(reg.clone());
    let mut req = empty_req();
    req.messages.push(json!({"role": "user", "content": "hi"}));
    req.messages.push(json!({
        "role": "assistant",
        "text": "done",
        "tool_calls": [],
    }));

    let d = hook.before_llm(&req, &ctx(id)).await;
    let Decision::Replace(new_req) = d else {
        panic!("expected Replace");
    };
    assert_eq!(new_req.messages.len(), 3);
    assert_eq!(new_req.messages[0]["content"], "hi");
    assert_eq!(new_req.messages[1]["text"], "done");
    assert_eq!(new_req.messages[2]["role"], "user");
    let text = new_req.messages[2]["content"][0]["text"]
        .as_str()
        .expect("text block");
    assert!(text.starts_with(MID_TURN_MARKER));
}

#[tokio::test]
async fn before_llm_splices_after_user_inside_toolchain() {
    let reg = InjectionRegistry::new();
    let id = RunId::new();
    reg.submit_user_text(&id, "msg3").await;
    let hook = InjectHook::new(reg.clone());
    let mut req = empty_req();
    req.messages.push(json!({"role": "user", "content": "u1"}));
    req.messages.push(assistant_tool_use("c1", "step"));
    req.messages.push(tool_result("c1", "ok"));
    req.messages.push(json!({"role": "user", "content": "u2"}));
    req.messages.push(assistant_empty());

    let d = hook.before_llm(&req, &ctx(id)).await;
    let Decision::Replace(new_req) = d else {
        panic!("expected Replace");
    };
    assert_eq!(new_req.messages.len(), 6);
    assert_eq!(new_req.messages[0]["content"], "u1");
    assert_eq!(new_req.messages[1]["role"], "assistant");
    assert_eq!(new_req.messages[2]["role"], "tool_result");
    assert_eq!(new_req.messages[3]["content"], "u2");
    assert_eq!(new_req.messages[4]["role"], "user");
    let injected = new_req.messages[4]["content"][0]["text"]
        .as_str()
        .expect("injected text");
    assert!(injected.starts_with(MID_TURN_MARKER));
    assert!(injected.ends_with("msg3"));
    assert_eq!(new_req.messages[5], assistant_empty());
}

#[test]
fn closing_empty_assistant_is_classified_as_tail() {
    let v = json!({"role": "assistant", "text": "", "tool_calls": []});
    assert!(is_unresolved_tail_value(&v));
}
