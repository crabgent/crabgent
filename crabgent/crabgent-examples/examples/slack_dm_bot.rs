//! Minimal Slack DM bot wiring example.
//!
//! Build with:
//! ```sh
//! cargo build --release --example slack_dm_bot
//! ```
//!
//! Run by providing Slack tokens and a target workspace/channel:
//! `SLACK_APP_TOKEN`, `SLACK_BOT_TOKEN`, `SLACK_WORKSPACE_ID`,
//! `SLACK_CHANNEL_ID`.

use std::sync::Arc;

use crabgent_channel::{Channel, OutboundMessage};
use crabgent_channel_slack::{SlackChannel, SlackConfig, SlackHttpClient};
use crabgent_core::{Owner, Subject};
use secrecy::SecretString;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app_token = required_env("SLACK_APP_TOKEN")?;
    let bot_token = required_env("SLACK_BOT_TOKEN")?;
    let workspace_id = required_env("SLACK_WORKSPACE_ID")?;
    let channel_id = required_env("SLACK_CHANNEL_ID")?;

    let config = SlackConfig::new(SecretString::from(app_token), SecretString::from(bot_token))?;
    let http = Arc::new(SlackHttpClient::new(config)?);
    let slack = SlackChannel::new(http);

    let subject = Subject::try_new("slack-dm-bot")?;
    let owner = Owner::new(format!("slack:{workspace_id}/{channel_id}"));
    let message = OutboundMessage::new("Hello from crabgent Slack.");
    slack.send(&subject, &owner, &message).await?;

    Ok(())
}

fn required_env(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(name)
}
