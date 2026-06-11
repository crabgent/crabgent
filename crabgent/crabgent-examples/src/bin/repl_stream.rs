//! Streaming variant of `repl-min`. Prints tokens as they arrive via
//! `Kernel::run_streaming`.
//!
//! Run with:
//! ```sh
//! cargo run -p crabgent-examples --bin repl-stream
//! ```

use crabgent_core::{
    AllowAllPolicy, ContentBlock, Event, Kernel, Message, RunId, RunRequest, Subject,
};
use crabgent_examples::ScriptedProvider;
use futures::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let kernel = Kernel::builder()
        .provider(ScriptedProvider::echo(
            "streamed: hello from the crabgent kernel",
        ))
        .policy(AllowAllPolicy)
        .build();

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    stdout
        .write_all(b"crabgent repl-stream. each token printed as it arrives. Ctrl+D to quit.\n> ")
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
            system_prompt: None,
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
        let events = kernel.run_streaming(req, None);
        tokio::pin!(events);
        while let Some(ev) = events.next().await {
            match ev {
                Ok(Event::Token(t)) => {
                    stdout.write_all(t.as_bytes()).await?;
                    stdout.flush().await?;
                }
                Ok(Event::Final(_)) => {
                    stdout.write_all(b"\n> ").await?;
                    stdout.flush().await?;
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    let line = format!("\nerror: {e}\n> ");
                    stdout.write_all(line.as_bytes()).await?;
                    stdout.flush().await?;
                    break;
                }
            }
        }
    }

    stdout.write_all(b"\nbye.\n").await?;
    stdout.flush().await?;
    Ok(())
}
