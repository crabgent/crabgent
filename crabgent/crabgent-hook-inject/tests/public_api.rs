use crabgent_core::{Decision, Hook, LlmRequest, RunCtx, RunId, Subject};
use crabgent_hook_inject::{InjectHook, InjectionRegistry};
use serde_json::json;

fn req_with_user_tail() -> LlmRequest {
    LlmRequest {
        model: "m".into(),
        system_prompt: None,
        messages: vec![
            json!({"role": "user", "content": "initial"}),
            json!({"role": "assistant", "text": "", "tool_calls": [{"id": "c1"}]}),
        ],
        tools: vec![],
        max_tokens: None,
        temperature: None,
        stop_sequences: vec![],
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
        tool_choice: None,
    }
}

#[tokio::test]
async fn public_registry_and_hook_splice_user_injection() {
    let registry = InjectionRegistry::default();
    let run_id = RunId::new();
    registry.submit_user_text(&run_id, "mid turn").await;
    registry.submit_system_text(&run_id, "system note").await;
    assert_eq!(registry.pending(&run_id).await, 2);

    let hook = InjectHook::new(registry.clone());
    let decision = hook
        .before_llm(
            &req_with_user_tail(),
            &RunCtx::new(run_id.clone(), Subject::new("subject")),
        )
        .await;
    let Decision::Replace(req) = decision else {
        panic!("expected replacement request");
    };

    assert_eq!(req.messages.len(), 4);
    assert_eq!(req.messages[1]["role"], "user");
    let text = req.messages[1]["content"][0]["text"]
        .as_str()
        .expect("injected user text");
    assert!(text.starts_with("[NEW USER INPUT mid-turn: address before ending this turn]"));
    assert_eq!(req.messages[2]["role"], "system");
    assert_eq!(req.messages[2]["content"], "system note");
    assert_eq!(registry.pending(&run_id).await, 0);
}
