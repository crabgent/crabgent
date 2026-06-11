//! Outbound Slack message mapping.

use crabgent_channel::OutboundMessage;

/// Arguments for Slack `chat.postMessage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackPostMessage {
    pub channel: String,
    pub text: String,
    pub thread_ts: Option<String>,
    pub reply_broadcast: bool,
    pub mrkdwn: bool,
}

/// Convert a generic channel outbound message into Slack post-message args.
#[must_use]
pub fn outbound_to_post_message(
    channel: &str,
    msg: &OutboundMessage,
    body_cap_chars: usize,
) -> SlackPostMessage {
    let mrkdwn = msg
        .metadata
        .get("mrkdwn")
        .is_none_or(|value| value != "false");
    let text = crabgent_core::text::truncate_chars(&msg.body, body_cap_chars);
    SlackPostMessage {
        channel: channel.to_owned(),
        text: format_slack_text(text, mrkdwn),
        thread_ts: msg
            .thread_parent
            .as_ref()
            .map(crabgent_channel::MessageRef::thread_root_or_id)
            .map(str::to_owned),
        reply_broadcast: msg
            .thread_parent
            .as_ref()
            .is_some_and(crabgent_channel::MessageRef::broadcast),
        mrkdwn,
    }
}

#[must_use]
pub(crate) fn format_slack_text(text: &str, mrkdwn: bool) -> String {
    if mrkdwn {
        markdown_to_slack_mrkdwn(text)
    } else {
        text.to_owned()
    }
}

fn markdown_to_slack_mrkdwn(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut first_line = true;
    let mut in_code_block = false;

    for line in text.lines() {
        if !first_line {
            out.push('\n');
        }
        first_line = false;

        if line.trim_start().starts_with("```") {
            in_code_block = !in_code_block;
            out.push_str(line);
            continue;
        }
        if in_code_block {
            out.push_str(line);
            continue;
        }
        if is_markdown_rule(line) {
            continue;
        }
        if let Some(heading) = markdown_heading_text(line) {
            out.push('*');
            push_slack_inline(&mut out, heading);
            out.push('*');
            continue;
        }
        push_slack_inline(&mut out, line);
    }

    out
}

fn is_markdown_rule(line: &str) -> bool {
    matches!(line.trim(), "---" | "***" | "___")
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

fn push_slack_inline(out: &mut String, text: &str) {
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('`')
            && let Some((inner, tail)) = after.split_once('`')
        {
            out.push('`');
            out.push_str(inner);
            out.push('`');
            rest = tail;
            continue;
        }
        if let Some(after) = rest.strip_prefix("**")
            && let Some((inner, tail)) = after.split_once("**")
        {
            out.push('*');
            out.push_str(inner);
            out.push('*');
            rest = tail;
            continue;
        }
        if let Some(after) = rest.strip_prefix("__")
            && let Some((inner, tail)) = after.split_once("__")
        {
            out.push('*');
            out.push_str(inner);
            out.push('*');
            rest = tail;
            continue;
        }
        if let Some(after) = rest.strip_prefix('[')
            && let Some((label, after_label)) = after.split_once("](")
            && let Some((url, tail)) = after_label.split_once(')')
        {
            out.push('<');
            out.push_str(url);
            out.push('|');
            push_slack_link_label(out, label);
            out.push('>');
            rest = tail;
            continue;
        }
        let mut chars = rest.chars();
        if let Some(ch) = chars.next() {
            out.push(ch);
            rest = chars.as_str();
        } else {
            break;
        }
    }
}

fn push_slack_link_label(out: &mut String, label: &str) {
    for ch in label.chars() {
        match ch {
            '|' | '>' => out.push(' '),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_capped_at_char_count() {
        let mut body = "a".repeat(40_000);
        body.push('ä');
        let msg = OutboundMessage::new(body);

        let post = outbound_to_post_message("C123", &msg, 40_000);

        assert_eq!(post.text.chars().count(), 40_000);
        assert!(post.text.is_char_boundary(post.text.len()));
        assert!(post.text.ends_with('a'));
    }

    #[test]
    fn mrkdwn_is_enabled_by_default() {
        let msg = OutboundMessage::new("*bold*");

        let post = outbound_to_post_message("C123", &msg, 40_000);

        assert!(post.mrkdwn);
    }

    #[test]
    fn mrkdwn_can_be_disabled_via_metadata() {
        let msg = OutboundMessage::new("**raw**").with_metadata("mrkdwn", "false");

        let post = outbound_to_post_message("C123", &msg, 40_000);

        assert!(!post.mrkdwn);
        assert_eq!(post.text, "**raw**");
    }

    #[test]
    fn common_markdown_is_normalized_to_slack_mrkdwn() {
        let msg = OutboundMessage::new("## Wetter\n**Regen** bei [DWD](https://dwd.de).");

        let post = outbound_to_post_message("C123", &msg, 40_000);

        assert_eq!(post.text, "*Wetter*\n*Regen* bei <https://dwd.de|DWD>.");
    }

    #[test]
    fn inline_code_is_not_rewritten_as_markdown() {
        let msg = OutboundMessage::new("Use `**literal**` then **bold**.");

        let post = outbound_to_post_message("C123", &msg, 40_000);

        assert_eq!(post.text, "Use `**literal**` then *bold*.");
    }
}
