//! `<perception .../>` tag rendering, escaping, and idempotent stripping.
//!
//! Mirrors the trust-fence discipline of `crabgent_prosody`'s `<voice>` tag: a
//! private `crabgent="1"` sentinel anchors the idempotent strip so the hook only
//! removes a block it injected itself, and the untrusted tone value is
//! attribute-escaped and length-bounded before it enters the prompt.

use crabgent_core::sanitize::xml_escape_body;
use crabgent_core::text::truncate_with_ellipsis;
use serde_json::{Value, json};

/// Leading marker of a hook-injected perception block. The private sentinel
/// lets the strip distinguish a hook-injected block from an attacker-influenced
/// `<perception ...>` token in the (untrusted) transcript body.
pub const PERCEPTION_SENTINEL: &str = "<perception crabgent=\"1\"";

/// Maximum byte length of the `tone` attribute value before truncation.
const TONE_ATTR_CAP: usize = 300;

/// Render the trust-fenced perception block carrying the audio model's tone
/// read. The tone is UNTRUSTED model output: it is attribute-escaped, then
/// length-bounded, before it enters the prompt.
pub fn render_perception_block(tone: &str) -> Value {
    let escaped = truncate_with_ellipsis(&escape_attr(tone), TONE_ATTR_CAP, "").into_owned();
    json!({
        "type": "text",
        "text": format!("{PERCEPTION_SENTINEL} conflict=\"text-vs-prosody\" tone=\"{escaped}\" />"),
    })
}

/// Drop every prior perception block from all user messages. Returns whether
/// any was removed, keeping the count bounded to one across repeated passes.
pub fn strip_prior_perception(messages: &mut [Value]) -> bool {
    let mut removed = false;
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        let before = content.len();
        content.retain(|block| !is_perception_block(block));
        removed |= content.len() != before;
    }
    removed
}

/// A `text` block carrying a previously injected perception tag.
fn is_perception_block(block: &Value) -> bool {
    block.get("type").and_then(Value::as_str) == Some("text")
        && block
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.starts_with(PERCEPTION_SENTINEL))
}

/// Escape for a double-quoted attribute: XML-escape `<`, `>`, `&`, then
/// neutralize the quote that would otherwise close the attribute.
fn escape_attr(s: &str) -> String {
    xml_escape_body(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    #[test]
    fn renders_self_closing_sentinel_tag() {
        let block = render_perception_block("flat, bitter tone");
        assert_eq!(block["type"], "text");
        let text = block["text"].as_str().expect("text");
        assert!(text.starts_with(PERCEPTION_SENTINEL), "sentinel: {text}");
        assert!(text.contains("conflict=\"text-vs-prosody\""));
        assert!(text.contains("tone=\"flat, bitter tone\""));
        assert!(text.ends_with("/>"), "self-closing: {text}");
    }

    #[test]
    fn hostile_tone_is_attribute_escaped() {
        let block = render_perception_block("\"><script>alert(1)</script>");
        let text = block["text"].as_str().expect("text");
        let value = text
            .split_once("tone=\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(value, _)| value)
            .expect("tone attribute");
        assert!(!value.contains('<'), "no raw < in {value}");
        assert!(value.contains("&quot;"), "quote escaped in {value}");
        assert!(value.contains("&lt;"), "lt escaped in {value}");
    }

    #[test]
    fn tone_value_is_length_bounded() {
        let block = render_perception_block(&"a".repeat(1000));
        let text = block["text"].as_str().expect("text");
        let value = text
            .split_once("tone=\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(value, _)| value)
            .expect("tone attribute");
        assert!(value.len() <= TONE_ATTR_CAP, "bounded: {}", value.len());
    }

    #[test]
    fn strip_removes_only_sentinel_blocks() {
        let mut messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": format!("{PERCEPTION_SENTINEL} tone=\"x\" />")},
                {"type": "text", "text": "real user text"},
                {"type": "text", "text": "<perception fake> not ours"},
            ],
        })];
        let removed = strip_prior_perception(&mut messages);
        assert!(removed);
        let content = messages[0]["content"].as_array().expect("content");
        assert_eq!(content.len(), 2, "only the sentinel block dropped");
        assert_eq!(content[0]["text"], "real user text");
        assert_eq!(content[1]["text"], "<perception fake> not ours");
    }

    #[test]
    fn strip_is_noop_without_sentinel_block() {
        let mut messages = vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": "hi"}],
        })];
        assert!(!strip_prior_perception(&mut messages));
    }
}
