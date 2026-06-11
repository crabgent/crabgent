//! Free helpers for the Slack [`crate::channel::SlackChannel`] adapter.
//!
//! Extracted from `channel.rs` to keep that file under the 500-LOC cap after
//! the `conv_display` addition. Pure mapping/transport helpers with no adapter
//! state.

use crabgent_channel::{ChannelError, MessageRef, ParticipantId, ReadMessage};
use crabgent_core::owner::Owner;
use reqwest::Client;
use std::str::FromStr;

use crate::api::SlackMessage;
use crate::ids::SlackOwner;
use crate::{CHANNEL_NAME, SlackError};

pub fn parse_owner(conv: &Owner) -> Result<SlackOwner, ChannelError> {
    SlackOwner::from_str(conv.as_str())
        .map_err(|err| ChannelError::InvalidEnvelope(format!("invalid Slack owner format: {err}")))
}

pub fn strip_emoji_colons(emoji: &str) -> &str {
    emoji
        .strip_prefix(':')
        .and_then(|value| value.strip_suffix(':'))
        .filter(|value| !value.is_empty())
        .unwrap_or(emoji)
}

pub fn clamp_read_limit(limit: usize) -> u32 {
    u32::try_from(limit).unwrap_or(100).clamp(1, 100)
}

pub fn read_message_from_slack(
    conv: &Owner,
    thread_parent: Option<&MessageRef>,
    message: SlackMessage,
) -> Option<ReadMessage> {
    let ts = message.ts?;
    let message_ref = match thread_parent {
        Some(parent) => {
            MessageRef::thread_reply(CHANNEL_NAME, conv.clone(), ts.clone(), &parent.id)
        }
        None => MessageRef::top_level(CHANNEL_NAME, conv.clone(), ts.clone()),
    };
    Some(ReadMessage {
        message_ref,
        author: ParticipantId::new(
            message
                .user
                .or(message.bot_id)
                .unwrap_or_else(|| "unknown".to_owned()),
        ),
        body: message.text.unwrap_or_default(),
        timestamp_unix_ms: slack_ts_to_unix_ms(&ts),
    })
}

pub fn slack_ts_to_unix_ms(ts: &str) -> i64 {
    let Some((seconds, micros)) = ts.split_once('.') else {
        return ts.parse::<i64>().map_or(0, |seconds| seconds * 1000);
    };
    let seconds = seconds.parse::<i64>().unwrap_or(0);
    let mut millis = micros.chars().take(3).collect::<String>();
    while millis.len() < 3 {
        millis.push('0');
    }
    let millis = millis.parse::<i64>().unwrap_or(0);
    seconds * 1000 + millis
}

pub async fn upload_bytes(
    client: &Client,
    upload_url: &str,
    bytes: Vec<u8>,
) -> Result<(), ChannelError> {
    // Slack's pre-signed upload URL only accepts POST. Using PUT returns
    // a 200 but Slack silently drops the body and the follow-up
    // files.completeUploadExternal then claims ok=true while leaving
    // `shares: {}` empty (file exists in the workspace but is never
    // posted into the channel).
    let status = client
        .post(upload_url)
        .body(bytes)
        .send()
        .await
        .map_err(|_err| ChannelError::adapter("Slack upload URL transport error"))?
        .status();
    if status.is_success() {
        Ok(())
    } else {
        Err(ChannelError::adapter(format!(
            "Slack upload URL returned HTTP {status}"
        )))
    }
}

pub fn slack_error(error: &SlackError) -> ChannelError {
    ChannelError::adapter(error)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use reqwest::Client;

    use super::{strip_emoji_colons, upload_bytes};

    #[test]
    fn strip_emoji_colons_only_for_wrapped_names() {
        assert_eq!(strip_emoji_colons(":eyes:"), "eyes");
        assert_eq!(strip_emoji_colons("eyes"), "eyes");
        assert_eq!(strip_emoji_colons(":eyes"), ":eyes");
        assert_eq!(strip_emoji_colons("eyes:"), "eyes:");
        assert_eq!(strip_emoji_colons("::"), "::");
    }

    #[tokio::test]
    async fn upload_bytes_redacts_presigned_url_on_transport_error() {
        let client = Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .expect("client");

        let err = upload_bytes(
            &client,
            "ftp://upload.example.test/path?secret=presigned-token",
            b"body".to_vec(),
        )
        .await
        .expect_err("unsupported scheme should fail before network I/O");

        let detail = match err {
            crabgent_channel::ChannelError::Adapter(detail) => detail,
            other => panic!("expected adapter error, got {other:?}"),
        };
        assert_eq!(detail, "Slack upload URL transport error");
        assert!(!detail.contains("presigned-token"));
        assert!(!detail.contains("upload.example.test"));
    }
}
