//! Table tests for the pure dangling-tool-call repair helper.

use super::*;
use chrono::Utc;
use crabgent_core::ContentBlock;
use crabgent_core::types::ToolCall;
use crabgent_store::Owner;
use serde_json::json;

fn user(text: &str) -> Message {
    Message::User {
        content: vec![ContentBlock::Text { text: text.into() }],
        timestamp: None,
    }
}

fn assistant_with_calls(calls: &[(&str, &str)]) -> Message {
    Message::Assistant {
        text: "working".into(),
        tool_calls: calls
            .iter()
            .map(|(id, name)| ToolCall {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
                args: json!({}),
                thought_signature: None,
            })
            .collect(),
    }
}

fn tool_result(call_id: &str) -> Message {
    Message::ToolResult {
        call_id: call_id.into(),
        output: json!("ok"),
        is_error: false,
    }
}

fn child_task(name: &str, status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        id: TaskId::new(),
        owner: Owner::new("u"),
        name: Some(name.into()),
        prompt: "p".into(),
        status,
        output: String::new(),
        error: None,
        created_at: now,
        updated_at: now,
        finished_at: None,
        parent_session_id: None,
        parent_task_id: None,
        context_mode: None,
        reasoning_effort_override: None,
        resume_spec: None,
        resume_count: 0,
        pause_cause: None,
        paused_at: None,
    }
}

#[test]
fn no_dangling_calls_is_a_no_op() {
    let transcript = vec![
        user("hi"),
        assistant_with_calls(&[("c1", "bash")]),
        tool_result("c1"),
    ];
    let (repaired, changed) = repair_dangling(transcript.clone(), &[]);
    assert!(!changed);
    assert_eq!(repaired.len(), transcript.len());
}

#[test]
fn dangling_call_gets_synthetic_error_result() {
    let transcript = vec![user("hi"), assistant_with_calls(&[("c1", "bash")])];
    let (repaired, changed) = repair_dangling(transcript, &[]);
    assert!(changed);
    match repaired.last() {
        Some(Message::ToolResult {
            call_id,
            output,
            is_error,
        }) => {
            assert_eq!(call_id, "c1");
            assert!(is_error);
            let text = output.as_str().expect("string note");
            assert!(text.contains("interrupted by shutdown/restart"));
            assert!(text.contains("partially executed"));
        }
        other => panic!("expected synthetic tool result, got {other:?}"),
    }
}

#[test]
fn partial_multi_call_turn_repairs_only_missing_ids() {
    let transcript = vec![
        user("hi"),
        assistant_with_calls(&[("c1", "bash"), ("c2", "bash"), ("c3", "bash")]),
        tool_result("c2"),
    ];
    let (repaired, changed) = repair_dangling(transcript, &[]);
    assert!(changed);
    let synthetic: Vec<&str> = repaired
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult {
                call_id,
                is_error: true,
                ..
            } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(synthetic, vec!["c1", "c3"]);
}

#[test]
fn task_tool_call_embeds_child_ids_for_reattach() {
    let child = child_task("worker", TaskStatus::Done);
    let transcript = vec![user("hi"), assistant_with_calls(&[("c1", "task")])];
    let (repaired, _changed) = repair_dangling(transcript, std::slice::from_ref(&child));
    let Some(Message::ToolResult { output, .. }) = repaired.last() else {
        panic!("expected synthetic tool result");
    };
    let text = output.as_str().expect("string note");
    assert!(text.contains("task get"), "re-attach hint present: {text}");
    assert!(text.contains(&child.id.to_string()));
    assert!(text.contains("worker"));
    assert!(text.contains("done"));
}

#[test]
fn non_task_tool_does_not_embed_children() {
    let child = child_task("worker", TaskStatus::Running);
    let transcript = vec![user("hi"), assistant_with_calls(&[("c1", "bash")])];
    let (repaired, _changed) = repair_dangling(transcript, std::slice::from_ref(&child));
    let Some(Message::ToolResult { output, .. }) = repaired.last() else {
        panic!("expected synthetic tool result");
    };
    let text = output.as_str().expect("string note");
    assert!(!text.contains(&child.id.to_string()));
}

#[test]
fn repair_is_idempotent_on_repaired_transcript() {
    let transcript = vec![user("hi"), assistant_with_calls(&[("c1", "task")])];
    let child = child_task("worker", TaskStatus::Running);
    let (repaired_once, changed_once) = repair_dangling(transcript, std::slice::from_ref(&child));
    assert!(changed_once);
    let (repaired_twice, changed_twice) =
        repair_dangling(repaired_once.clone(), std::slice::from_ref(&child));
    assert!(!changed_twice, "second repair is a no-op");
    assert_eq!(repaired_twice.len(), repaired_once.len());
}

#[test]
fn order_children_first_sorts_deepest_first() {
    use crabgent_core::policy::AllowAllPolicy;
    use crabgent_test_support::StubProvider;
    let kernel = Arc::new(
        crabgent_core::Kernel::builder()
            .provider(StubProvider::with_text("x"))
            .policy(AllowAllPolicy)
            .build(),
    );
    let mut root = child_task("root", TaskStatus::Paused);
    root.created_at = Utc::now();
    let mut mid = child_task("mid", TaskStatus::Paused);
    mid.parent_task_id = Some(root.id.clone());
    let mut leaf = child_task("leaf", TaskStatus::Paused);
    leaf.parent_task_id = Some(mid.id.clone());

    let ordered = order_children_first(vec![
        (root, Arc::clone(&kernel)),
        (mid, Arc::clone(&kernel)),
        (leaf, Arc::clone(&kernel)),
    ]);
    let names: Vec<&str> = ordered
        .iter()
        .map(|(task, _)| task.name.as_deref().unwrap_or("-"))
        .collect();
    assert_eq!(names, vec!["leaf", "mid", "root"]);
}
