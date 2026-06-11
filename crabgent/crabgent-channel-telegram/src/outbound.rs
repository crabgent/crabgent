use crabgent_channel::{ChannelError, MessageRef, OutboundMessage, ParticipantId};
use crabgent_core::owner::Owner;
use serde::Serialize;
use serde_json::{Value, json};

const PARSE_MODE_HTML: &str = "HTML";

#[derive(Debug, Serialize)]
pub struct SendMessageBody {
    chat_id: i64,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i64>,
}

pub fn build_send_message_body(
    chat_id: i64,
    msg: &OutboundMessage,
    body_cap_chars: usize,
) -> Result<SendMessageBody, ChannelError> {
    let thread_id = parse_thread_id(msg.thread_parent.as_ref())?;
    let text = crabgent_core::text::truncate_chars(&msg.body, body_cap_chars);
    let formatted = format_telegram_text(text);
    Ok(SendMessageBody {
        chat_id,
        text: formatted.text,
        parse_mode: formatted.parse_mode,
        message_thread_id: thread_id,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramFormattedText {
    pub text: String,
    pub parse_mode: Option<&'static str>,
}

pub fn format_telegram_text(text: &str) -> TelegramFormattedText {
    if contains_supported_html_tag(text) {
        return TelegramFormattedText {
            text: text.to_owned(),
            parse_mode: Some(PARSE_MODE_HTML),
        };
    }
    if let Some(html) = markdown_to_telegram_html(text) {
        return TelegramFormattedText {
            text: html,
            parse_mode: Some(PARSE_MODE_HTML),
        };
    }
    TelegramFormattedText {
        text: text.to_owned(),
        parse_mode: None,
    }
}

fn contains_supported_html_tag(text: &str) -> bool {
    let mut remaining = text;
    while let Some((_, after_start)) = remaining.split_once('<') {
        remaining = after_start;
        let tag = after_start.strip_prefix('/').unwrap_or(after_start);
        if let Some(name) = html_tag_name(tag)
            && is_supported_html_tag(name)
        {
            return true;
        }
    }
    false
}

fn html_tag_name(text: &str) -> Option<&str> {
    let name = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .next()
        .unwrap_or("");
    if name.is_empty() { None } else { Some(name) }
}

fn is_supported_html_tag(name: &str) -> bool {
    matches!(
        name,
        "a" | "b"
            | "blockquote"
            | "code"
            | "del"
            | "em"
            | "i"
            | "ins"
            | "pre"
            | "s"
            | "span"
            | "strike"
            | "strong"
            | "tg-emoji"
            | "tg-spoiler"
            | "tg-time"
            | "u"
    )
}

fn markdown_to_telegram_html(text: &str) -> Option<String> {
    let mut html = String::with_capacity(text.len());
    let mut converted = false;
    let mut first_line = true;
    let mut in_code_block = false;
    let mut code_block_has_language = false;

    for line in text.lines() {
        if !first_line {
            html.push('\n');
        }
        first_line = false;

        if let Some(info) = line.trim_start().strip_prefix("```") {
            converted = true;
            if in_code_block {
                close_pre(&mut html, code_block_has_language);
                in_code_block = false;
                code_block_has_language = false;
            } else {
                code_block_has_language = open_pre(&mut html, info);
                in_code_block = true;
            }
            continue;
        }

        if in_code_block {
            push_html_escaped(&mut html, line);
            continue;
        }

        if let Some(heading) = markdown_heading_text(line) {
            converted = true;
            html.push_str("<b>");
            push_telegram_inline(&mut html, heading);
            html.push_str("</b>");
            continue;
        }

        converted |= push_telegram_inline(&mut html, line);
    }

    if in_code_block {
        close_pre(&mut html, code_block_has_language);
    }

    converted.then_some(html)
}

fn open_pre(out: &mut String, info: &str) -> bool {
    let Some(language) = telegram_language(info) else {
        out.push_str("<pre>");
        return false;
    };
    out.push_str("<pre><code class=\"language-");
    push_html_attr_escaped(out, &language);
    out.push_str("\">");
    true
}

fn close_pre(out: &mut String, has_language: bool) {
    if has_language {
        out.push_str("</code></pre>");
    } else {
        out.push_str("</pre>");
    }
}

fn telegram_language(info: &str) -> Option<String> {
    let language: String = info
        .split_whitespace()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '+'))
        .collect();
    if language.is_empty() {
        None
    } else {
        Some(language)
    }
}

fn markdown_heading_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for prefix in ["###### ", "##### ", "#### ", "### ", "## ", "# "] {
        if let Some(text) = trimmed.strip_prefix(prefix) {
            return Some(text.trim());
        }
    }
    None
}

fn push_telegram_inline(out: &mut String, text: &str) -> bool {
    let mut rest = text;
    let mut converted = false;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**")
            && let Some((inner, tail)) = after.split_once("**")
        {
            converted = true;
            push_wrapped_html(out, "b", inner);
            rest = tail;
            continue;
        }
        if let Some(after) = rest.strip_prefix("__")
            && let Some((inner, tail)) = after.split_once("__")
        {
            converted = true;
            push_wrapped_html(out, "b", inner);
            rest = tail;
            continue;
        }
        if let Some(after) = rest.strip_prefix('`')
            && let Some((inner, tail)) = after.split_once('`')
        {
            converted = true;
            push_wrapped_html(out, "code", inner);
            rest = tail;
            continue;
        }
        if let Some(after) = rest.strip_prefix('[')
            && let Some((label, after_label)) = after.split_once("](")
            && let Some((url, tail)) = after_label.split_once(')')
        {
            converted = true;
            out.push_str("<a href=\"");
            push_html_attr_escaped(out, url);
            out.push_str("\">");
            push_html_escaped(out, label);
            out.push_str("</a>");
            rest = tail;
            continue;
        }
        if let Some(after) = rest.strip_prefix('*')
            && let Some((inner, tail)) = after.split_once('*')
        {
            converted = true;
            push_wrapped_html(out, "i", inner);
            rest = tail;
            continue;
        }
        if let Some(after) = rest.strip_prefix('_')
            && let Some((inner, tail)) = after.split_once('_')
        {
            converted = true;
            push_wrapped_html(out, "i", inner);
            rest = tail;
            continue;
        }
        let mut chars = rest.chars();
        if let Some(ch) = chars.next() {
            push_html_escaped_char(out, ch);
            rest = chars.as_str();
        } else {
            break;
        }
    }
    converted
}

fn push_wrapped_html(out: &mut String, tag: &str, text: &str) {
    out.push('<');
    out.push_str(tag);
    out.push('>');
    push_html_escaped(out, text);
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn push_html_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        push_html_escaped_char(out, ch);
    }
}

fn push_html_attr_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("&quot;"),
            _ => push_html_escaped_char(out, ch),
        }
    }
}

fn push_html_escaped_char(out: &mut String, ch: char) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        _ => out.push(ch),
    }
}

pub fn parse_chat_id(conv: &Owner) -> Result<i64, ChannelError> {
    conv.as_str()
        .strip_prefix("telegram:")
        .unwrap_or(conv.as_str())
        .parse::<i64>()
        .map_err(|_err| ChannelError::ConversationNotFound(conv.as_str().to_owned()))
}

/// Parse a `ParticipantId` carrying a Telegram user/chat id into a raw `i64`.
///
/// Accepts the bare numeric form returned by the poller (`"12345"`) and the
/// `participants()` form prefixed with `"user:"`. Other shapes return
/// `ChannelError::InvalidEnvelope`.
pub fn parse_chat_id_from_participant(recipient: &ParticipantId) -> Result<i64, ChannelError> {
    let raw = recipient.as_str();
    let digits = raw.strip_prefix("user:").unwrap_or(raw);
    digits
        .parse::<i64>()
        .map_err(|err| ChannelError::InvalidEnvelope(format!("invalid telegram user id: {err}")))
}

pub fn parse_thread_id(parent: Option<&MessageRef>) -> Result<Option<i64>, ChannelError> {
    let Some(parent) = parent else {
        return Ok(None);
    };
    let Some(root) = parent.thread_root.as_deref() else {
        return Ok(None);
    };
    root.parse::<i64>().map(Some).map_err(|err| {
        ChannelError::InvalidEnvelope(format!("non-numeric thread_root '{root}': {err}"))
    })
}

pub fn parse_message_id(parent_id: &str) -> Result<i64, ChannelError> {
    parent_id.parse::<i64>().map_err(|err| {
        ChannelError::InvalidEnvelope(format!("non-numeric message id '{parent_id}': {err}"))
    })
}

pub fn build_set_message_reaction_body(chat_id: i64, message_id: i64, emoji: &str) -> Value {
    json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "reaction": [{
            "type": "emoji",
            "emoji": emoji
        }]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_send_message_body_caps_by_char_count() {
        let msg = OutboundMessage::new("äbc".repeat(20));

        let body = build_send_message_body(42, &msg, 10).expect("test result");

        assert_eq!(body.chat_id, 42);
        assert_eq!(body.parse_mode, None);
        assert_eq!(body.text.chars().count(), 10);
        assert!(body.text.is_char_boundary(body.text.len()));
    }

    #[test]
    fn build_send_message_body_sets_html_parse_mode_for_supported_tags() {
        let msg = OutboundMessage::new("Use <code>Main Street</code>.");

        let body = build_send_message_body(42, &msg, 4096).expect("test result");

        assert_eq!(body.parse_mode, Some("HTML"));
    }

    #[test]
    fn build_send_message_body_converts_basic_markdown_to_telegram_html() {
        let msg = OutboundMessage::new("## Wetter\n**Regen** bei [DWD](https://dwd.de).");

        let body = build_send_message_body(42, &msg, 4096).expect("test result");

        assert_eq!(body.parse_mode, Some("HTML"));
        assert_eq!(
            body.text,
            "<b>Wetter</b>\n<b>Regen</b> bei <a href=\"https://dwd.de\">DWD</a>."
        );
    }

    #[test]
    fn format_telegram_text_escapes_markdown_content_when_using_html_mode() {
        let body = format_telegram_text("`2 < 3 & ok`");

        assert_eq!(body.parse_mode, Some("HTML"));
        assert_eq!(body.text, "<code>2 &lt; 3 &amp; ok</code>");
    }

    #[test]
    fn parse_chat_id_strips_telegram_prefix() {
        assert_eq!(
            parse_chat_id(&Owner::new("telegram:42")).expect("test result"),
            42
        );
        assert_eq!(parse_chat_id(&Owner::new("42")).expect("test result"), 42);
        parse_chat_id(&Owner::new("telegram:abc")).expect_err("expected error");
    }

    #[test]
    fn parse_thread_id_handles_none_some_invalid() {
        assert_eq!(parse_thread_id(None).expect("test result"), None);
        let parent_top = MessageRef::top_level("telegram", Owner::new("telegram:1"), "1");
        assert_eq!(
            parse_thread_id(Some(&parent_top)).expect("test result"),
            None
        );
        let parent_thread = MessageRef::thread_reply_broadcast(
            "telegram",
            Owner::new("telegram:1"),
            "1",
            "99",
            false,
        );
        assert_eq!(
            parse_thread_id(Some(&parent_thread)).expect("test result"),
            Some(99)
        );
        let parent_bad = MessageRef::thread_reply_broadcast(
            "telegram",
            Owner::new("telegram:1"),
            "1",
            "x",
            false,
        );
        parse_thread_id(Some(&parent_bad)).expect_err("expected error");
    }

    #[test]
    fn reaction_body_matches_telegram_api_shape() {
        let body = build_set_message_reaction_body(42, 1700, "👀");

        assert_eq!(
            body,
            json!({
                "chat_id": 42,
                "message_id": 1700,
                "reaction": [{
                    "type": "emoji",
                    "emoji": "👀"
                }]
            })
        );
    }

    #[test]
    fn parse_message_id_rejects_non_numeric_parent_id() {
        let Ok(message_id) = parse_message_id("1700") else {
            panic!("numeric telegram message id should parse");
        };
        assert_eq!(message_id, 1700);
        assert!(matches!(
            parse_message_id("abc"),
            Err(ChannelError::InvalidEnvelope(_))
        ));
    }

    #[test]
    fn parse_chat_id_from_participant_handles_both_forms() {
        assert_eq!(
            parse_chat_id_from_participant(&ParticipantId::new("12345")).expect("test result"),
            12345
        );
        assert_eq!(
            parse_chat_id_from_participant(&ParticipantId::new("user:67890")).expect("test result"),
            67890
        );
        assert!(matches!(
            parse_chat_id_from_participant(&ParticipantId::new("not-a-number")),
            Err(ChannelError::InvalidEnvelope(_))
        ));
        assert!(matches!(
            parse_chat_id_from_participant(&ParticipantId::new("user:abc")),
            Err(ChannelError::InvalidEnvelope(_))
        ));
    }
}
