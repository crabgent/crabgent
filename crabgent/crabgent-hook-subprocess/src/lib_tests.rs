//! Tests for the public `SubprocessHook` `Hook` impl. Lives in a
//! separate file so the lib source stays under the 500-LoC cap.

use super::*;
use crabgent_core::{RunId, Subject};

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("u1"))
}

fn sh(script: &str) -> Vec<String> {
    vec!["sh".into(), "-c".into(), script.into()]
}

fn note() -> Notification {
    Notification {
        kind: "info".into(),
        message: "hi".into(),
        level: crabgent_core::NotificationLevel::Info,
    }
}

#[tokio::test]
async fn continue_when_subprocess_returns_continue() {
    let hook =
        SubprocessHook::builder(sh(r#"cat > /dev/null; printf '{"decision":"continue"}\n'"#))
            .build();
    let d = hook.on_session_start(&ctx()).await;
    assert!(matches!(d, Decision::Continue));
}

#[tokio::test]
async fn deny_propagates_with_reason() {
    let hook = SubprocessHook::builder(sh(
        r#"cat > /dev/null; printf '{"decision":"deny","reason":"nope"}\n'"#,
    ))
    .build();
    let d = hook
        .before_tool(
            &ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                args: serde_json::json!({}),
                thought_signature: None,
            },
            &ctx(),
        )
        .await;
    match d {
        Decision::Deny(r) => assert_eq!(r, "nope"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn replace_decodes_typed_payload() {
    let payload = r#"{"decision":"replace","value":{"id":"c2","name":"echo","args":{"x":1}}}"#;
    let cmd = sh(&format!(r"cat > /dev/null; printf '{payload}\n'"));
    let hook = SubprocessHook::builder(cmd).build();
    let d = hook
        .before_tool(
            &ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                args: serde_json::json!({}),
                thought_signature: None,
            },
            &ctx(),
        )
        .await;
    match d {
        Decision::Replace(call) => {
            assert_eq!(call.id, "c2");
            assert_eq!(call.name, "echo");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn malformed_replace_payload_yields_deny() {
    let cmd = sh(r#"cat > /dev/null; printf '{"decision":"replace","value":{"bad":1}}\n'"#);
    let hook = SubprocessHook::builder(cmd).build();
    let d = hook
        .before_tool(
            &ToolCall {
                id: "c1".into(),
                name: "bash".into(),
                args: serde_json::json!({}),
                thought_signature: None,
            },
            &ctx(),
        )
        .await;
    assert!(matches!(d, Decision::Deny(s) if s.starts_with("malformed replace")));
}

#[tokio::test]
async fn strict_mode_yields_deny_on_subprocess_error() {
    let hook = SubprocessHook::builder(vec!["definitely-not-a-real-binary".to_string()])
        .failure_mode(FailureMode::Strict)
        .build();
    let d = hook.on_session_start(&ctx()).await;
    assert!(matches!(d, Decision::Deny(s) if s.contains("subprocess hook error")));
}

#[tokio::test]
async fn lenient_mode_yields_continue_on_subprocess_error() {
    let hook = SubprocessHook::builder(vec!["definitely-not-a-real-binary".to_string()])
        .failure_mode(FailureMode::Lenient)
        .build();
    let d = hook.on_session_start(&ctx()).await;
    assert!(matches!(d, Decision::Continue));
}

#[tokio::test]
async fn timeout_classified_as_failure_mode() {
    let hook = SubprocessHook::builder(sh("sleep 5"))
        .timeout(Duration::from_millis(80))
        .failure_mode(FailureMode::Strict)
        .build();
    let d = hook.on_session_start(&ctx()).await;
    assert!(matches!(d, Decision::Deny(s) if s.contains("subprocess hook error")));
}

#[tokio::test]
async fn on_session_start_dispatches_event_to_subprocess() {
    // The script denies only when it actually received the on_session_start
    // envelope on stdin; a default no-op forward would Continue instead.
    let hook = SubprocessHook::builder(sh(
        r#"input=$(cat); case "$input" in *'"event":"on_session_start"'*) printf '{"decision":"deny","reason":"saw event"}\n';; *) printf '{"decision":"continue"}\n';; esac"#,
    ))
    .failure_mode(FailureMode::Strict)
    .build();
    let d = hook.on_session_start(&ctx()).await;
    assert!(matches!(d, Decision::Deny(s) if s == "saw event"));
}

#[tokio::test]
async fn only_events_filter_routes_on_session_start() {
    // Filtering to on_session_start lets that event through to the script.
    let hook = SubprocessHook::builder(sh(
        r#"cat > /dev/null; printf '{"decision":"deny","reason":"blocked"}\n'"#,
    ))
    .only_events(["on_session_start"])
    .build();
    let d = hook.on_session_start(&ctx()).await;
    assert!(matches!(d, Decision::Deny(s) if s == "blocked"));
}

#[tokio::test]
async fn only_events_filter_short_circuits_other_events() {
    // Subprocess would deny if it ran. A Continue proves the adapter
    // never spawned it.
    let hook = SubprocessHook::builder(sh(
        r#"cat > /dev/null; printf '{"decision":"deny","reason":"unreachable"}\n'"#,
    ))
    .only_events(["before_llm"])
    .build();
    let d = hook.on_session_start(&ctx()).await;
    assert!(matches!(d, Decision::Continue));
}

#[tokio::test]
async fn only_events_filter_routes_listed_events() {
    let hook = SubprocessHook::builder(sh(
        r#"cat > /dev/null; printf '{"decision":"deny","reason":"blocked"}\n'"#,
    ))
    .only_events(["before_llm"])
    .build();
    let req = LlmRequest {
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
    };
    let d = hook.before_llm(&req, &ctx()).await;
    assert!(matches!(d, Decision::Deny(s) if s == "blocked"));
}

#[tokio::test]
async fn unit_dispatch_treats_replace_as_continue() {
    // Unit-typed Decisions (on_session_start, on_stop, ...) ignore Replace
    // because there is no payload to substitute.
    let hook = SubprocessHook::builder(sh(
        r#"cat > /dev/null; printf '{"decision":"replace","value":null}\n'"#,
    ))
    .build();
    let d = hook.on_session_start(&ctx()).await;
    assert!(matches!(d, Decision::Continue));
}

#[tokio::test]
async fn pre_compact_replace_decodes_messages() {
    let payload = r#"{"decision":"replace","value":[{"role":"system","content":"compact"}]}"#;
    let hook =
        SubprocessHook::builder(sh(&format!(r"cat > /dev/null; printf '{payload}\n'"))).build();
    let msgs = vec![Message::User {
        content: vec![crabgent_core::ContentBlock::Text { text: "hi".into() }],
        timestamp: None,
    }];
    let d = hook.pre_compact(&msgs, &ctx()).await;
    match d {
        Decision::Replace(next) => {
            assert_eq!(next.len(), 1);
            assert!(matches!(next[0], Message::System { .. }));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn on_notification_dispatches_payload() {
    let hook = SubprocessHook::builder(sh(
        r#"input=$(cat); echo "$input" >&2; printf '{"decision":"continue"}\n'"#,
    ))
    .build();
    let d = hook.on_notification(&note(), &ctx()).await;
    assert!(matches!(d, Decision::Continue));
}

#[tokio::test]
async fn on_event_replace_decodes_into_event() {
    // Subprocess returns a Replace whose value is a serialised Event.
    // The adapter must deserialise it back into the typed enum.
    let payload = r#"{"decision":"replace","value":{"kind":"token","data":"hi"}}"#;
    let cmd = sh(&format!(r"cat > /dev/null; printf '{payload}\n'"));
    let hook = SubprocessHook::builder(cmd).build();
    let d = hook.on_event(&Event::Token("orig".into()), &ctx()).await;
    match d {
        Decision::Replace(Event::Token(t)) => assert_eq!(t, "hi"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn on_message_dispatches_message_slice() {
    let cmd = sh(r#"cat > /dev/null; printf '{"decision":"continue"}\n'"#);
    let hook = SubprocessHook::builder(cmd).build();
    let msgs = vec![Message::System {
        content: "be helpful".into(),
    }];
    let d = hook.on_message(&msgs, &ctx()).await;
    assert!(matches!(d, Decision::Continue));
}

#[tokio::test]
async fn after_tool_serializes_call_and_result() {
    let cmd = sh(r#"cat > /dev/null; printf '{"decision":"continue"}\n'"#);
    let hook = SubprocessHook::builder(cmd).build();
    let call = ToolCall {
        id: "c1".into(),
        name: "bash".into(),
        args: serde_json::json!({}),
        thought_signature: None,
    };
    let result = ToolResult {
        call_id: "c1".into(),
        output: serde_json::json!({"ok": true}),
        is_error: false,
        run_messages: Vec::new(),
    };
    let d = hook.after_tool(&call, &result, &ctx()).await;
    assert!(matches!(d, Decision::Continue));
}
