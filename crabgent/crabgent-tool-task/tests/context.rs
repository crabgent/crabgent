mod common;

use std::sync::{Arc, Mutex};

use crabgent_core::Tool;
use crabgent_store::{SessionId, TaskId, TaskStore};
use serde_json::json;

use common::{ImmediateProvider, build_harness, create_args, ctx, session_id_string};

#[tokio::test]
async fn context_messages_text_image_toolresult_wired_into_taskrequest() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h = build_harness(
        ImmediateProvider::recording("done", Arc::clone(&seen)),
        crabgent_task::TaskExecutor::new,
        Arc::new(crabgent_core::AllowAllPolicy),
    );
    let mut args = create_args("ignored when context exists");
    args["block"] = json!(true);
    args["context"] = json!({
        "context_messages": [
            {
                "role": "user",
                "content": [
                    {"type": "text", "text": "hello"},
                    {"type": "image", "mime": "image/png", "data": "AQID"}
                ]
            },
            {
                "role": "tool_result",
                "call_id": "call-1",
                "output": {"ok": true},
                "is_error": false
            }
        ]
    });

    h.tool.execute(args, &ctx()).await.expect("create succeeds");

    let reqs = seen.lock().expect("seen lock");
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].messages.len(), 2);
    assert_eq!(reqs[0].messages[0]["role"], "user");
    assert_eq!(reqs[0].messages[0]["content"][0]["text"], "hello");
    assert_eq!(reqs[0].messages[0]["content"][1]["type"], "image");
    assert_eq!(reqs[0].messages[1]["role"], "tool_result");
    assert_eq!(reqs[0].messages[1]["call_id"], "call-1");
}

#[tokio::test]
async fn context_mode_parent_session_id_system_prompt_forwarded_unchanged() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let h = build_harness(
        ImmediateProvider::recording("done", Arc::clone(&seen)),
        crabgent_task::TaskExecutor::new,
        Arc::new(crabgent_core::AllowAllPolicy),
    );
    let session = session_id_string();
    let mut args = create_args("with metadata");
    args["block"] = json!(true);
    args["context"] = json!({
        "parent_session_id": session,
        "context_mode": "recent_thread",
        "system_prompt": "stay brief"
    });

    let out = h.tool.execute(args, &ctx()).await.expect("create succeeds");

    let id: TaskId = out["task_id"]
        .as_str()
        .expect("task id")
        .parse()
        .expect("value should parse");
    let task = h
        .store
        .get(&id)
        .await
        .expect("test result")
        .expect("test result");
    assert_eq!(
        task.parent_session_id,
        Some(session.parse::<SessionId>().expect("test result"))
    );
    assert_eq!(task.context_mode.as_deref(), Some("recent_thread"));
    let reqs = seen.lock().expect("seen lock");
    assert_eq!(reqs[0].system_prompt.as_deref(), Some("stay brief"));
}
