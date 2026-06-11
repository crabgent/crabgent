//! Minimal stdin REPL using the kernel's non-streaming `run()` API.
//!
//! Run with:
//! ```sh
//! cargo run -p crabgent-examples --bin repl-min
//! ```
//!
//! Type a line, press enter, see a canned response. Ctrl+D to quit.

use crabgent_core::{AllowAllPolicy, ContentBlock, Kernel, Message, RunId, RunRequest, Subject};
use crabgent_examples::ScriptedProvider;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::echo(
            "echoed: hello from the crabgent kernel",
        ))
        .policy(AllowAllPolicy)
        .build();

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    stdout
        .write_all(b"crabgent repl-min. type a line and press enter. Ctrl+D to quit.\n> ")
        .await?;
    stdout.flush().await?;

    while let Some(line) = reader.next_line().await? {
        if line.is_empty() {
            stdout.write_all(b"> ").await?;
            stdout.flush().await?;
            continue;
        }
        let req = RunRequest {
            pause: None,
            run_id: RunId::new(),
            subject: Subject::try_new("repl-user")?,
            model: "scripted".into(),
            explicit_model: None,
            session_model_override: None,
            fallbacks: Vec::new(),
            system_prompt: Some("You are a friendly demo assistant.".into()),
            messages: vec![Message::User {
                content: vec![ContentBlock::Text { text: line }],
                timestamp: None,
            }],
            max_turns: Some(2),
            temperature: None,
            max_tokens: None,
            cancel_reason: None,
            reasoning_effort: None,
            web_search: ::crabgent_core::types::WebSearchConfig::default(),
        };
        match kernel.run(req, None).await {
            Ok(text) => {
                stdout.write_all(text.as_bytes()).await?;
                stdout.write_all(b"\n> ").await?;
            }
            Err(e) => {
                let line = format!("error: {e}\n> ");
                stdout.write_all(line.as_bytes()).await?;
            }
        }
        stdout.flush().await?;
    }

    stdout.write_all(b"\nbye.\n").await?;
    stdout.flush().await?;
    Ok(())
}
