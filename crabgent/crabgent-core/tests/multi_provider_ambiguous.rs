use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, KernelError, Message, ModelInfo, ModelTarget, RunId,
    RunRequest, Subject,
};
use crabgent_test_support::StubProvider;

fn same_model_provider(provider: &'static str, text: &'static str) -> StubProvider {
    StubProvider::with_text(text)
        .with_name(provider)
        .with_models(vec![ModelInfo::minimal("opus", provider)])
}

fn kernel() -> Kernel {
    Kernel::builder()
        .provider(same_model_provider("anthropic", "anthropic"))
        .provider(same_model_provider("openai", "openai"))
        .policy(AllowAllPolicy)
        .build()
}

fn request(model: ModelTarget) -> RunRequest {
    RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::new("user"),
        model,
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text { text: "hi".into() }],
            timestamp: None,
        }],
        system_prompt: None,
        max_turns: Some(1),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::WebSearchConfig::default(),
    }
}

#[tokio::test]
async fn unqualified_duplicate_model_is_ambiguous_at_runtime() {
    let err = kernel()
        .run(request(ModelTarget::id("opus")), None)
        .await
        .expect_err("unqualified duplicate model is ambiguous");
    match err {
        KernelError::AmbiguousModel(id) => assert_eq!(id.as_str(), "opus"),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn provider_qualified_duplicate_model_routes_to_provider() {
    let anthropic = kernel()
        .run(request(ModelTarget::new("anthropic", "opus")), None)
        .await
        .expect("anthropic target routes");
    let openai = kernel()
        .run(request(ModelTarget::new("openai", "opus")), None)
        .await
        .expect("openai target routes");

    assert_eq!(anthropic, "anthropic");
    assert_eq!(openai, "openai");
}
