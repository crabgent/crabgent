//! Defines a custom `Tool` impl and registers it with the kernel.
//! The scripted provider issues a tool-use response on the first turn,
//! then the kernel dispatches the tool, feeds the result back, and the
//! provider closes with `EndTurn`.
//!
//! Run with:
//! ```sh
//! cargo run -p crabgent-examples --bin custom-tool
//! ```

use async_trait::async_trait;
use std::io::{self, Write};

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Kernel, Message, RunId, RunRequest, Subject, Tool, ToolCtx,
    ToolError,
};
use crabgent_examples::ScriptedProvider;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
struct Args {
    n: i64,
}

/// A simple custom tool: returns the square of an integer argument.
struct SquareTool;

#[async_trait]
impl Tool for SquareTool {
    fn name(&self) -> &'static str {
        "square"
    }

    fn description(&self) -> &'static str {
        "Square an integer. Caller passes {\"n\": <int>}."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"n": {"type": "integer"}},
            "required": ["n"],
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolCtx) -> Result<Value, ToolError> {
        let parsed: Args = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArgs(format!("expected {{n: int}}: {e}")))?;
        let squared = parsed.n.saturating_mul(parsed.n);
        Ok(json!({"n": parsed.n, "squared": squared}))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::tool_then_final(
            "square",
            json!({"n": 7}),
            "square(7) is 49 (per the tool result).",
        ))
        .policy(AllowAllPolicy)
        .add_tool(SquareTool)
        .build();

    let req = RunRequest {
        pause: None,
        run_id: RunId::new(),
        subject: Subject::try_new("demo-user")?,
        model: "scripted".into(),
        explicit_model: None,
        session_model_override: None,
        fallbacks: Vec::new(),
        system_prompt: Some("Use tools when needed.".into()),
        messages: vec![Message::User {
            content: vec![ContentBlock::Text {
                text: "what is 7 squared?".into(),
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

    let final_text = kernel.run(req, None).await?;
    writeln!(io::stdout(), "{final_text}")?;
    Ok(())
}
