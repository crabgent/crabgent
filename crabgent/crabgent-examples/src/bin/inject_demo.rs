//! Demonstrates `crabgent_hook_inject`. The main task starts a run; a
//! sibling `tokio::spawn` waits 200ms then injects an extra user
//! message into the registry. The next iteration's `before_llm`
//! callback sees the message; the response acknowledges it.
//!
//! Run with:
//! ```sh
//! cargo run -p crabgent-examples --bin inject-demo
//! ```

use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, LlmRequest, LlmResponse, Message, Provider,
    ProviderCapabilities, ProviderError, RunCtx, RunId, RunRequest, StopReason, Subject, Usage,
    model::ModelInfo,
};
use crabgent_hook_inject::{InjectHook, InjectionRegistry};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Provider that summarises every incoming `LlmRequest`. Each call
/// reports how many messages the request carried; if we observe the
/// count growing across calls, the injection has happened.
struct CountingProvider;

#[async_trait::async_trait]
impl Provider for CountingProvider {
    async fn complete(
        &self,
        req: &LlmRequest,
        _ctx: &RunCtx,
        _cancel: Option<&CancellationToken>,
    ) -> Result<LlmResponse, ProviderError> {
        let last_msg = req
            .messages
            .last()
            .and_then(extract_text)
            .unwrap_or_else(|| "(no text)".into());
        let text = format!(
            "[counting-provider] saw {} messages, last text: {last_msg}",
            req.messages.len(),
        );
        Ok(LlmResponse {
            text,
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            model: "counting".into(),
        })
    }

    fn name(&self) -> &'static str {
        "counting"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tools: true,
            ..Default::default()
        }
    }

    fn models(&self) -> Vec<ModelInfo> {
        vec![ModelInfo::minimal("counting", "counting")]
    }
}

fn extract_text(msg: &Value) -> Option<String> {
    if let Some(text) = msg.get("text").and_then(|v| v.as_str()) {
        return Some(text.to_string());
    }
    if let Some(content) = msg.get("content") {
        if let Some(s) = content.as_str() {
            return Some(s.to_string());
        }
        if let Some(arr) = content.as_array() {
            for item in arr {
                if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = InjectionRegistry::new();
    let inject_hook = InjectHook::new(registry.clone());

    let kernel = Arc::new(
        Kernel::builder()
            .provider(CountingProvider)
            .policy(AllowAllPolicy)
            .add_hook(inject_hook)
            .build(),
    );

    let run_id = RunId::new();

    // Sibling task: waits a moment, then nudges the running run.
    let inject_id = run_id.clone();
    let inject_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        registry
            .submit_user_text(
                &inject_id,
                "INJECTED: please remember the magic word is 'pretzel'",
            )
            .await;
        writeln!(io::stdout(), "[demo] submitted injection into registry")
    });

    let req = RunRequest {
        pause: None,
        run_id: run_id.clone(),
        subject: Subject::try_new("demo-user")?,
        model: "counting".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        system_prompt: None,
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "hi, please introduce yourself".into(),
            }],
            timestamp: None,
        }],
        max_turns: Some(3),
        temperature: None,
        max_tokens: None,
        cancel_reason: None,
        reasoning_effort: None,
        web_search: crabgent_core::types::WebSearchConfig::default(),
    };

    // Slight delay so the injection lands before the second iteration
    // would normally fire. With our scripted provider this is academic
    // (max_turns=3 means we make up to three LLM calls), but the demo
    // is clearer when injection wins the race for at least one call.
    tokio::time::sleep(Duration::from_millis(120)).await;

    let final_text = kernel.run(req, None).await?;
    inject_task.await??;
    writeln!(io::stdout(), "[demo] final response:\n{final_text}")?;
    Ok(())
}
