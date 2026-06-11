//! Outbound Matrix message helpers.

use crabgent_channel::{ChannelError, OutboundMessage, ParticipantId};
use crabgent_core::owner::Owner;
use matrix_sdk::ruma::{
    OwnedEventId, OwnedRoomId, OwnedUserId,
    events::{
        relation::Thread,
        room::message::{Relation, RoomMessageEventContent},
    },
};

/// Stable adapter name used by the Matrix channel.
pub const CHANNEL_NAME: &str = "matrix";

/// Parse a crabgent conversation owner into a Matrix room id.
pub fn parse_owner_to_room_id(conv: &Owner) -> Result<OwnedRoomId, ChannelError> {
    let raw = conv
        .as_str()
        .strip_prefix("matrix:")
        .unwrap_or_else(|| conv.as_str());
    OwnedRoomId::try_from(raw.to_owned())
        .map_err(|err| ChannelError::InvalidOwnerFormat(format!("invalid Matrix room id: {err}")))
}

/// Parse a `ParticipantId` carrying a Matrix user id (`@user:server`)
/// into an `OwnedUserId`.
pub fn parse_recipient_to_user_id(recipient: &ParticipantId) -> Result<OwnedUserId, ChannelError> {
    OwnedUserId::try_from(recipient.as_str().to_owned())
        .map_err(|err| ChannelError::InvalidEnvelope(format!("invalid Matrix user id: {err}")))
}

/// Convert an outbound message to `m.room.message` content.
pub fn build_text_content_with_thread(
    msg: &OutboundMessage,
    body_cap_bytes: usize,
) -> Result<RoomMessageEventContent, ChannelError> {
    let body = crabgent_core::text::truncate_bytes_at_boundary(&msg.body, body_cap_bytes);
    let mut content = build_text_content(body);
    if let Some(parent) = msg.thread_parent.as_ref() {
        let root = parent.thread_root_or_id();
        let root_id = OwnedEventId::try_from(root.to_owned()).map_err(|err| {
            ChannelError::InvalidEnvelope(format!("invalid matrix thread root '{root}': {err}"))
        })?;
        let reply_id = OwnedEventId::try_from(parent.id.clone()).map_err(|err| {
            ChannelError::InvalidEnvelope(format!(
                "invalid matrix reply target '{}': {err}",
                parent.id
            ))
        })?;
        content.relates_to = Some(Relation::Thread(Thread::reply(root_id, reply_id)));
    }
    Ok(content)
}

pub fn build_text_content(body: &str) -> RoomMessageEventContent {
    if contains_matrix_html_tag(body) {
        return RoomMessageEventContent::text_html(plain_text_from_html(body), body.to_owned());
    }
    if let Some(html) = unicode_bullets_to_html(body) {
        return RoomMessageEventContent::text_html(body.to_owned(), html);
    }
    RoomMessageEventContent::text_markdown(body.to_owned())
}

fn contains_matrix_html_tag(text: &str) -> bool {
    let mut remaining = text;
    while let Some((_, after_start)) = remaining.split_once('<') {
        remaining = after_start;
        let tag = after_start.strip_prefix('/').unwrap_or(after_start);
        if let Some(name) = html_tag_name(tag)
            && is_matrix_html_tag(name)
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

fn is_matrix_html_tag(name: &str) -> bool {
    matches!(
        name,
        "a" | "b"
            | "blockquote"
            | "br"
            | "caption"
            | "code"
            | "del"
            | "details"
            | "div"
            | "em"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "hr"
            | "i"
            | "img"
            | "li"
            | "ol"
            | "p"
            | "pre"
            | "s"
            | "span"
            | "strong"
            | "sub"
            | "summary"
            | "sup"
            | "table"
            | "tbody"
            | "td"
            | "th"
            | "thead"
            | "tr"
            | "u"
            | "ul"
    )
}

fn unicode_bullets_to_html(body: &str) -> Option<String> {
    let mut html = String::new();
    let mut in_list = false;
    let mut converted = false;
    for line in body.lines() {
        if let Some(item) = unicode_bullet_item(line) {
            if !in_list {
                html.push_str("<ul>\n");
                in_list = true;
            }
            html.push_str("<li>");
            push_escaped_html(&mut html, item.trim());
            html.push_str("</li>\n");
            converted = true;
        } else {
            if in_list {
                html.push_str("</ul>\n");
                in_list = false;
            }
            if let Some((tag, text)) = markdown_heading(line) {
                push_tagged_html(&mut html, tag, text);
                html.push('\n');
            } else if !line.trim().is_empty() {
                push_escaped_html(&mut html, line.trim());
                html.push_str("<br />\n");
            }
        }
    }
    if in_list {
        html.push_str("</ul>\n");
    }
    converted.then_some(html)
}

fn unicode_bullet_item(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    trimmed
        .strip_prefix("• ")
        .or_else(|| trimmed.strip_prefix("◦ "))
        .or_else(|| trimmed.strip_prefix("‣ "))
}

fn markdown_heading(line: &str) -> Option<(&'static str, &str)> {
    let trimmed = line.trim_start();
    if let Some(text) = trimmed.strip_prefix("###### ") {
        Some(("h6", text.trim()))
    } else if let Some(text) = trimmed.strip_prefix("##### ") {
        Some(("h5", text.trim()))
    } else if let Some(text) = trimmed.strip_prefix("#### ") {
        Some(("h4", text.trim()))
    } else if let Some(text) = trimmed.strip_prefix("### ") {
        Some(("h3", text.trim()))
    } else if let Some(text) = trimmed.strip_prefix("## ") {
        Some(("h2", text.trim()))
    } else if let Some(text) = trimmed.strip_prefix("# ") {
        Some(("h1", text.trim()))
    } else {
        None
    }
}

fn push_tagged_html(out: &mut String, tag: &str, text: &str) {
    out.push('<');
    out.push_str(tag);
    out.push('>');
    push_escaped_html(out, text);
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn plain_text_from_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    loop {
        let Some((before, after_start)) = rest.split_once('<') else {
            push_decoded_entities(&mut out, rest);
            break;
        };
        push_decoded_entities(&mut out, before);
        let Some((tag, after_tag)) = after_start.split_once('>') else {
            push_decoded_entities(&mut out, after_start);
            break;
        };
        if is_text_break_tag(tag) {
            push_line_break(&mut out);
        }
        rest = after_tag;
    }
    out.trim().to_owned()
}

fn is_text_break_tag(tag: &str) -> bool {
    let tag = tag
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("");
    matches!(tag, "br" | "div" | "li" | "p" | "tr")
}

fn push_line_break(out: &mut String) {
    if !out.ends_with('\n') {
        out.push('\n');
    }
}

fn push_decoded_entities(out: &mut String, text: &str) {
    out.push_str(
        &text
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
            .replace("&quot;", "\""),
    );
}

fn push_escaped_html(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crabgent_channel::MessageRef;
    use matrix_sdk::ruma::events::room::message::MessageType;

    fn text_parts(content: RoomMessageEventContent) -> (String, Option<String>) {
        let MessageType::Text(text) = content.msgtype else {
            panic!("expected text content");
        };
        (text.body, text.formatted.map(|formatted| formatted.body))
    }

    #[test]
    fn owner_parser_accepts_matrix_prefix() {
        let room =
            parse_owner_to_room_id(&Owner::new("matrix:!abc:example.org")).expect("test result");
        assert_eq!(room.as_str(), "!abc:example.org");
    }

    #[test]
    fn owner_parser_rejects_bad_room_id_as_owner_format() {
        assert!(matches!(
            parse_owner_to_room_id(&Owner::new("matrix:not-a-room-id")),
            Err(ChannelError::InvalidOwnerFormat(_))
        ));
    }

    #[test]
    fn thread_content_sets_thread_relation() {
        let conv = Owner::new("matrix:!room:example.org");
        let parent = MessageRef::top_level(CHANNEL_NAME, conv, "$root:example.org");
        let msg = OutboundMessage::new("reply").in_thread(parent);
        let content = build_text_content_with_thread(&msg, 65_536).expect("test result");
        assert!(matches!(content.relates_to, Some(Relation::Thread(_))));
    }

    #[test]
    fn invalid_thread_root_is_rejected() {
        let conv = Owner::new("matrix:!room:example.org");
        let parent = MessageRef::thread_reply_broadcast(
            CHANNEL_NAME,
            conv,
            "$reply:example.org",
            "bad",
            false,
        );
        let msg = OutboundMessage::new("reply").in_thread(parent);
        assert!(matches!(
            build_text_content_with_thread(&msg, 65_536),
            Err(ChannelError::InvalidEnvelope(_))
        ));
    }

    #[test]
    fn top_level_content_has_no_relation() {
        let msg = OutboundMessage::new("hello");
        let content = build_text_content_with_thread(&msg, 65_536).expect("test result");
        assert!(content.relates_to.is_none());
    }

    #[test]
    fn html_body_uses_matrix_custom_html_with_plain_fallback() {
        let content = build_text_content("<strong>Main Street</strong><br />Nearby");
        let (body, formatted) = text_parts(content);

        assert_eq!(body, "Main Street\nNearby");
        assert_eq!(
            formatted.as_deref(),
            Some("<strong>Main Street</strong><br />Nearby")
        );
    }

    #[test]
    fn unicode_bullets_become_real_matrix_list_html() {
        let content = build_text_content("### Wetter\n• Regen\n• Wind < stark");
        let (body, formatted) = text_parts(content);

        assert_eq!(body, "### Wetter\n• Regen\n• Wind < stark");
        assert_eq!(
            formatted.as_deref(),
            Some("<h3>Wetter</h3>\n<ul>\n<li>Regen</li>\n<li>Wind &lt; stark</li>\n</ul>\n")
        );
    }

    #[test]
    fn markdown_body_still_uses_sdk_markdown_conversion() {
        let content = build_text_content("**fett** und _kursiv_");
        let (_body, formatted) = text_parts(content);

        assert_eq!(
            formatted.as_deref(),
            Some("<strong>fett</strong> und <em>kursiv</em>")
        );
    }

    #[test]
    fn body_is_capped_at_byte_count() {
        let msg = OutboundMessage::new("ä".repeat(40_000));
        let content = build_text_content_with_thread(&msg, 65_536).expect("test result");

        let MessageType::Text(text) = content.msgtype else {
            panic!("expected text content");
        };
        assert!(text.body.len() <= 65_536);
        assert!(text.body.is_char_boundary(text.body.len()));
    }

    #[test]
    fn recipient_parser_accepts_full_user_id() {
        let user =
            parse_recipient_to_user_id(&ParticipantId::new("@alice:example.org")).expect("ok");
        assert_eq!(user.as_str(), "@alice:example.org");
    }

    #[test]
    fn recipient_parser_rejects_garbage() {
        assert!(matches!(
            parse_recipient_to_user_id(&ParticipantId::new("not-a-user-id")),
            Err(ChannelError::InvalidEnvelope(_))
        ));
    }
}
