//! `TelegramChannel`: Direct-only `Channel` implementation.
//!
//! Wraps the Telegram Bot API as a [`Channel`] adapter. Always
//! reports `ChannelKind::Direct` and `DirectRole::HumanAgent`:
//! Group/Supergroup/Channel conversations are out-of-scope for this
//! pass.
//!
//! Threading is mapped opaquely:
//! `OutboundMessage.thread_parent.thread_root` becomes the
//! `message_thread_id` field on `sendMessage`. This matches the
//! Forum-Topic semantics of supergroups; for plain Direct chats
//! `thread_root` is `None` and no thread id is sent.

use async_trait::async_trait;
use crabgent_channel::{
    Channel, ChannelError, ChannelKind, DirectRole, MessageRef, OutboundMessage, Participant,
    ParticipantId, ParticipantRole,
};
use crabgent_core::owner::Owner;
use crabgent_core::subject::Subject;
use crabgent_log::{debug, instrument};
use reqwest::multipart;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::outbound;

/// Stable adapter name returned from `Channel::name`.
pub const CHANNEL_NAME: &str = "telegram";

const DEFAULT_API_BASE: &str = "https://api.telegram.org";
const DEFAULT_BODY_CAP_CHARS: usize = 4096;

/// Direct-only Telegram bot adapter.
pub struct TelegramChannel {
    bot_token: SecretString,
    api_base: String,
    bot_username: String,
    bot_user_id: String,
    client: reqwest::Client,
    body_cap_chars: usize,
}

impl TelegramChannel {
    /// Build an adapter from a bot token. The bot identity
    /// (`user_id` and username, used in `participants()`) is
    /// supplied separately so callers can avoid an upfront `getMe`
    /// call in tests; production setups typically discover them
    /// once at startup and pass them in. Use [`TelegramChannel::with_client`]
    /// when you want to share a `reqwest::Client` across multiple
    /// bot instances.
    pub fn new(
        bot_token: impl Into<String>,
        bot_user_id: impl Into<String>,
        bot_username: impl Into<String>,
    ) -> Self {
        Self {
            bot_token: SecretString::from(bot_token.into()),
            api_base: DEFAULT_API_BASE.into(),
            bot_username: bot_username.into(),
            bot_user_id: bot_user_id.into(),
            client: crate::http::build_bot_api_client()
                .expect("hardened bot api client has no fallible configuration"),
            body_cap_chars: DEFAULT_BODY_CAP_CHARS,
        }
    }

    /// Override the API base URL (for tests against `httpmock`).
    #[must_use]
    pub fn with_api_base(mut self, base: impl Into<String>) -> Self {
        self.api_base = base.into();
        self
    }

    /// Override the underlying `reqwest::Client`.
    #[must_use]
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// Override the outbound body cap.
    #[must_use]
    pub const fn with_body_cap_chars(mut self, body_cap_chars: usize) -> Self {
        self.body_cap_chars = body_cap_chars;
        self
    }

    fn endpoint(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{}",
            self.api_base,
            self.bot_token.expose_secret(),
            method
        )
    }

    fn adapter_error(&self, msg: impl std::fmt::Display) -> ChannelError {
        ChannelError::adapter(redact_bot_token(&msg.to_string(), &self.bot_token))
    }

    fn reqwest_error(&self, err: reqwest::Error) -> ChannelError {
        self.adapter_error(err.without_url())
    }

    /// Borrow the bot token.
    #[must_use]
    pub const fn bot_token(&self) -> &SecretString {
        &self.bot_token
    }

    /// Borrow the configured API base URL.
    #[must_use]
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    pub(crate) async fn post_json(
        &self,
        method: &str,
        body: &Value,
    ) -> Result<Value, ChannelError> {
        let url = self.endpoint(method);
        let resp = self
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|err| self.reqwest_error(err))?;
        self.decode_response(method, resp).await
    }

    pub(crate) async fn post_multipart(
        &self,
        method: &str,
        form: multipart::Form,
    ) -> Result<Value, ChannelError> {
        let url = self.endpoint(method);
        let resp = self
            .client
            .post(url)
            .multipart(form)
            .send()
            .await
            .map_err(|err| self.reqwest_error(err))?;
        self.decode_response(method, resp).await
    }

    async fn decode_response(
        &self,
        method: &str,
        resp: reqwest::Response,
    ) -> Result<Value, ChannelError> {
        if !resp.status().is_success() {
            return Err(self.adapter_error(format!(
                "telegram api {method} returned status {}",
                resp.status()
            )));
        }
        let value: Value = resp.json().await.map_err(|err| self.reqwest_error(err))?;
        if value.get("ok").and_then(Value::as_bool) != Some(true) {
            let desc = value
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_owned();
            return Err(self.adapter_error(format!("telegram api {method} not ok: {desc}")));
        }
        Ok(value)
    }
}

fn redact_bot_token(message: &str, token: &SecretString) -> String {
    let token = token.expose_secret();
    if token.is_empty() || !message.contains(token) {
        return message.to_owned();
    }
    message.replace(token, "<redacted>")
}

#[derive(Debug, Deserialize)]
struct SendMessageResult {
    message_id: i64,
    chat: SendMessageChat,
    #[serde(default)]
    message_thread_id: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SendMessageChat {
    id: i64,
}

fn parse_send_message_result(
    value: &Value,
    method: &str,
) -> Result<SendMessageResult, ChannelError> {
    serde_json::from_value(value.get("result").cloned().ok_or_else(|| {
        ChannelError::adapter(format!("telegram {method} response missing result"))
    })?)
    .map_err(ChannelError::Serde)
}

fn message_ref_from_send_result(
    result: &SendMessageResult,
    thread_parent: Option<&MessageRef>,
) -> MessageRef {
    let conv = Owner::new(format!("telegram:{}", result.chat.id));
    let id = result.message_id.to_string();
    if let Some(root) = thread_parent
        .map(|parent| parent.thread_root_or_id().to_owned())
        .or_else(|| {
            result
                .message_thread_id
                .map(|thread_id| thread_id.to_string())
        })
    {
        return MessageRef::thread_reply_broadcast(CHANNEL_NAME, conv, id, root, false);
    }
    MessageRef::top_level(CHANNEL_NAME, conv, id)
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &'static str {
        CHANNEL_NAME
    }

    async fn kind(&self, _conv: &Owner) -> Result<ChannelKind, ChannelError> {
        Ok(ChannelKind::Direct)
    }

    async fn participants(
        &self,
        _ctx: &Subject,
        conv: &Owner,
    ) -> Result<Vec<Participant>, ChannelError> {
        let chat_id = outbound::parse_chat_id(conv)?;
        let bot = Participant::new(ParticipantId::new(&self.bot_user_id), ParticipantRole::Bot)
            .with_display_name(&self.bot_username);
        let user = Participant::new(
            ParticipantId::new(format!("user:{chat_id}")),
            ParticipantRole::Human,
        );
        Ok(vec![bot, user])
    }

    #[instrument(level = "debug", skip(self, _ctx, msg), fields(conv = %conv))]
    async fn send(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let chat_id = outbound::parse_chat_id(conv)?;
        let body = outbound::build_send_message_body(chat_id, msg, self.body_cap_chars)?;
        let body_json = serde_json::to_value(&body).map_err(ChannelError::Serde)?;
        debug!(method = "sendMessage", "telegram send dispatch");
        let value = self.post_json("sendMessage", &body_json).await?;
        let result = parse_send_message_result(&value, "sendMessage")?;
        Ok(message_ref_from_send_result(
            &result,
            msg.thread_parent.as_ref(),
        ))
    }

    async fn edit(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        let chat_id = outbound::parse_chat_id(conv)?;
        let message_id = outbound::parse_message_id(&target.id)?;
        let formatted = outbound::format_telegram_text(new_text);
        let mut body = serde_json::Map::new();
        body.insert("chat_id".to_owned(), json!(chat_id));
        body.insert("message_id".to_owned(), json!(message_id));
        body.insert("text".to_owned(), json!(formatted.text));
        if let Some(parse_mode) = formatted.parse_mode {
            body.insert("parse_mode".to_owned(), json!(parse_mode));
        }
        let body = Value::Object(body);
        debug!(method = "editMessageText", "telegram edit dispatch");
        self.post_json("editMessageText", &body).await?;
        Ok(())
    }

    async fn delete(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        let chat_id = outbound::parse_chat_id(conv)?;
        let message_id = outbound::parse_message_id(&target.id)?;
        let body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
        });
        debug!(method = "deleteMessage", "telegram delete dispatch");
        self.post_json("deleteMessage", &body).await?;
        Ok(())
    }

    async fn upload(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        filename: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
        thread_parent: Option<&MessageRef>,
    ) -> Result<MessageRef, ChannelError> {
        let chat_id = outbound::parse_chat_id(conv)?;
        let part = multipart::Part::bytes(bytes).file_name(filename.to_owned());
        let mut form = multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);
        if let Some(comment) = comment {
            let formatted = outbound::format_telegram_text(comment);
            form = form.text("caption", formatted.text);
            if let Some(parse_mode) = formatted.parse_mode {
                form = form.text("parse_mode", parse_mode);
            }
        }
        if let Some(parent) = thread_parent {
            // Direct-only uploads use a plain reply link here, not a
            // forum-topic thread_root.
            let message_id = outbound::parse_message_id(&parent.id)?;
            form = form.text("reply_to_message_id", message_id.to_string());
        }
        debug!(method = "sendDocument", "telegram upload dispatch");
        let value = self.post_multipart("sendDocument", form).await?;
        let result = parse_send_message_result(&value, "sendDocument")?;
        Ok(message_ref_from_send_result(&result, thread_parent))
    }

    async fn react(
        &self,
        _ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        crate::react::react(self, conv, parent, emoji).await
    }

    async fn notify_user(
        &self,
        _ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        let chat_id = outbound::parse_chat_id_from_participant(recipient)?;
        let mut top_level_msg = msg.clone();
        top_level_msg.thread_parent = None;
        let body = outbound::build_send_message_body(chat_id, &top_level_msg, self.body_cap_chars)?;
        let body_json = serde_json::to_value(&body).map_err(ChannelError::Serde)?;
        debug!(method = "sendMessage", "telegram notify_user dispatch");
        let value = self.post_json("sendMessage", &body_json).await?;
        let result = parse_send_message_result(&value, "sendMessage")?;
        Ok(message_ref_from_send_result(&result, None))
    }

    // Telegram Bot API has no channel-history endpoint; Channel::read stays on default Unsupported.
    async fn direct_role(&self, _conv: &Owner) -> Result<Option<DirectRole>, ChannelError> {
        Ok(Some(DirectRole::HumanAgent))
    }
}

#[cfg(test)]
mod tests;
