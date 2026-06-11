//! Conversation context hint helpers used by `KernelChannelInbox::build_request`.
//!
//! The hint template is English-only; locale override is out of scope.
//!
//! Inbound text crosses the adapter-to-kernel trust boundary once here.
//! `sanitize_for_prompt` keeps Unicode `General_Category` groups L (Letter),
//! N (Number), P (Punctuation), S (Symbol), and Zs (`Space_Separator`), then
//! XML-escapes body text. It blocks every other category, including Cc
//! controls such as LF/CR, Cf format controls such as zero-width joiners and
//! bidi overrides, Zl/Zp line and paragraph separators, Co private-use, Cn
//! unassigned/noncharacters, and M marks. Fullwidth lookalikes remain allowed
//! because they are printable punctuation/symbol/letter code points and no
//! NFKC normalization is applied.

use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};

use crate::channel::ChannelKind;
use crate::envelope::InboundEvent;
use crate::error::ChannelError;

/// Maximum byte size of an inbound message body before it is rejected.
///
/// Length is measured in BYTES (`str::len`), not code points, so it
/// lines up with tokenizer and provider request limits. Adapters call
/// [`check_inbound_size`] BEFORE [`sanitize_for_prompt`].
pub const INBOUND_BODY_MAX_BYTES: usize = 8192;

/// Reject an inbound body whose byte length exceeds [`INBOUND_BODY_MAX_BYTES`].
///
/// Returns `Err(ChannelError::InboundTooLarge { observed, max })` when
/// `input.len()` is over the cap, `Ok(())` otherwise. No silent
/// truncation: the caller decides whether to log + drop the event or
/// surface a refusal to the user.
pub const fn check_inbound_size(input: &str) -> Result<(), ChannelError> {
    if input.len() > INBOUND_BODY_MAX_BYTES {
        return Err(ChannelError::InboundTooLarge {
            observed: input.len(),
            max: INBOUND_BODY_MAX_BYTES,
        });
    }
    Ok(())
}

/// Sanitize an inbound body for inclusion in an LLM prompt.
///
/// Pipeline:
/// 1. Keep only Unicode `General_Category` groups L, N, P, S, and Zs.
/// 2. Minimal XML-escape on body content: `<` -> `&lt;`, `>` -> `&gt;`,
///    `&` -> `&amp;`. No other entity replacements.
///
/// NFKC normalization is intentionally NOT applied. Fullwidth lookalikes
/// such as U+FF1C, U+FF1E, and fullwidth Latin letters survive the pipeline
/// unchanged so consumer detection logic still sees the original code
/// points as a bypass-attempt signal.
pub fn sanitize_for_prompt(s: &str) -> String {
    xml_escape_body(&strip_denylisted(s))
}

/// Strip the prompt-injection denylist (Cc controls, Cf format chars such as
/// zero-width joiners and bidi overrides, Zl/Zp separators, Co/Cn other, and M
/// marks) WITHOUT applying any XML escape.
///
/// This is the half of [`sanitize_for_prompt`] that the `<inbound>` tag
/// attribute path needs: the attribute escape (`<`/`>`/`&`/`"`) is then applied
/// once by `crabgent_core::sanitize::sanitize_for_attribute`, so quotes are also
/// neutralized and `&` is escaped exactly once (no double-escape).
pub(super) fn strip_denylisted(s: &str) -> String {
    s.chars().filter(|c| is_allowed(*c)).collect()
}

fn is_allowed(c: char) -> bool {
    match c.general_category_group() {
        GeneralCategoryGroup::Letter
        | GeneralCategoryGroup::Number
        | GeneralCategoryGroup::Punctuation
        | GeneralCategoryGroup::Symbol => true,
        GeneralCategoryGroup::Separator => c.general_category() == GeneralCategory::SpaceSeparator,
        GeneralCategoryGroup::Mark | GeneralCategoryGroup::Other => false,
    }
}

fn xml_escape_body(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            other => out.push(other),
        }
    }
    out
}

fn sanitize_field(s: &str) -> String {
    let cleaned = sanitize_for_prompt(s);
    if cleaned.is_empty() {
        "(unknown)".to_owned()
    } else {
        cleaned
    }
}

pub(super) fn build_conversation_hint(
    event: &InboundEvent,
    inferred_kind: Option<ChannelKind>,
    channel_display: Option<&str>,
) -> String {
    build_conversation_hint_with_delivery(event, inferred_kind, channel_display, DeliveryHint::Tool)
}

pub(super) fn build_live_conversation_hint(
    event: &InboundEvent,
    inferred_kind: Option<ChannelKind>,
    channel_display: Option<&str>,
) -> String {
    build_conversation_hint_with_delivery(
        event,
        inferred_kind,
        channel_display,
        DeliveryHint::Final,
    )
}

#[derive(Debug, Clone, Copy)]
enum DeliveryHint {
    Tool,
    Final,
}

fn build_conversation_hint_with_delivery(
    event: &InboundEvent,
    inferred_kind: Option<ChannelKind>,
    channel_display: Option<&str>,
    delivery: DeliveryHint,
) -> String {
    // Prefer the readable channel/room name (the same value the `<inbound>`
    // tag renders as `name`) over the raw adapter slug. Falls back to the
    // slug when `Channel::conv_display` resolved no name.
    let channel = channel_display
        .map(sanitize_field)
        .filter(|name| name != "(unknown)")
        .unwrap_or_else(|| sanitize_field(event.channel.as_str()));
    let conv = sanitize_field(event.conv.as_str());
    // No raw-id leak where a display name is available: use the sender's
    // display name and only fall back to the channel-opaque id when the
    // adapter supplied no readable label.
    let sender = sanitize_field(
        event
            .from
            .display_name
            .as_deref()
            .unwrap_or_else(|| event.from.id.as_str()),
    );
    let role = sanitize_field(event.from.role.as_str());
    let delivery_hint = match delivery {
        DeliveryHint::Tool => format!(
            "To reply, call the channel_send tool with conv=\"{conv}\". Plain text in your final response is NOT delivered to the participant."
        ),
        DeliveryHint::Final => format!(
            "Reply by writing normal assistant text in your final response; the channel runtime delivers it to the participant. Use channel_send with conv=\"{conv}\" only for explicit extra channel messages."
        ),
    };
    match inferred_kind {
        Some(kind) => format!(
            "Conversation context: you are responding inside a {kind} conversation on the \"{channel}\" channel (conv=\"{conv}\"). Sender: \"{sender}\" (role=\"{role}\"). {delivery_hint}",
            kind = kind.as_str(),
        ),
        None => format!(
            "Conversation context: you are responding inside a conversation on the \"{channel}\" channel (conv=\"{conv}\"). Sender: \"{sender}\" (role=\"{role}\"). {delivery_hint}",
        ),
    }
}

#[cfg(test)]
#[path = "hint_tests.rs"]
mod tests;
