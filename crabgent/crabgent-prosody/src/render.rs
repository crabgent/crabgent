//! Tag rendering and escaping for [`crate::hook::ProsodyHook`].
//!
//! These functions turn a serialized [`crabgent_core::VoiceSignals`] JSON
//! object into a single self-closing `<voice .../>` tag. The tag is a
//! trust fence: every attribute value is attribute-escaped so a hostile
//! audio-event label cannot break out of the tag, and the joined event
//! list is length-bounded to cap token cost.

use serde_json::Value;

use crabgent_core::sanitize::xml_escape_body;

/// Open marker of the rendered tag. Used both as the tag prefix and to
/// detect a previously rendered tag for idempotent re-runs.
pub const VOICE_TAG_OPEN: &str = "<voice ";

/// Private sentinel attribute emitted as the first attribute of every
/// hook-injected `<voice .../>` tag. Idempotent strip-and-re-emit keys on
/// this exact prefix so the hook only ever removes a tag it injected
/// itself, never an attacker-influenced `<voice ...>`-shaped token that
/// happens to lead the (untrusted) STT transcript text.
const SENTINEL_ATTR: &str = "crabgent=\"1\"";

/// Leading marker of a hook-injected tag: open marker plus sentinel.
/// `strip_prior_voice_tag` removes a tag only when the text starts with
/// this exact prefix.
const INJECTED_TAG_PREFIX: &str = "<voice crabgent=\"1\"";

/// Maximum byte length of the joined `events="..."` attribute value
/// before truncation. Bounds token cost from pathological label lists.
pub const EVENTS_ATTR_CAP: usize = 200;

/// Maximum byte length of the joined speaker attribute value.
pub const SPEAKERS_ATTR_CAP: usize = 200;
/// Maximum byte length of the joined speaker identity attribute value.
pub const SPEAKER_IDENTITIES_ATTR_CAP: usize = 200;

/// Build the self-closing `<voice .../>` tag from a serialized
/// [`crabgent_core::VoiceSignals`] JSON object.
///
/// Every present attribute renders unconditionally: events (when the
/// label list is non-empty), `pause_ms`, `rate`, `hesitations` (when the
/// count is non-zero), and `energy`. A field absent from the `voice`
/// object is simply omitted. The compute layer ([`crate::voice_signals`])
/// already decided which signals exist via [`crate::ProsodyConfig`]; the
/// renderer does not re-gate that decision. A `None`-encoded field is
/// already missing from the JSON and so never appears in the tag.
///
/// The tag always leads with the private [`SENTINEL_ATTR`]
/// (`crabgent="1"`), so [`strip_prior_voice_tag`] can tell a hook-injected
/// tag from a user-authored `<voice ...>` in the untrusted transcript.
///
/// Returns `None` when no real signal attribute would be emitted, so a
/// fully empty signal set produces no tag (and no bare sentinel tag).
pub fn render_voice_tag(voice: &Value) -> Option<String> {
    let mut attrs: Vec<String> = Vec::new();

    if let Some(events) = events_attr(voice) {
        attrs.push(format!("events=\"{events}\""));
    }
    match speakers_attr(voice).as_deref() {
        Some([single]) => attrs.push(format!("speaker=\"{single}\"")),
        Some(many) => attrs.push(format!("speakers=\"{}\"", many.join(","))),
        None => {}
    }
    match speaker_identities_attr(voice).as_deref() {
        Some([single]) => {
            attrs.push(format!("identified_speaker=\"{}\"", single.label));
            attrs.push(format!("speaker_confidence=\"{}\"", single.confidence));
            attrs.push(format!("speaker_source=\"{}\"", single.source));
            if let Some(provider_speaker) = &single.provider_speaker {
                attrs.push(format!("speaker_label=\"{provider_speaker}\""));
            }
        }
        Some(many) => attrs.push(format!(
            "identified_speakers=\"{}\"",
            many.iter()
                .map(|identity| identity.label.as_str())
                .collect::<Vec<_>>()
                .join(",")
        )),
        None => {}
    }
    if let Some(pause) = voice.get("pause_ms").and_then(Value::as_u64) {
        attrs.push(format!("pause_ms=\"{pause}\""));
    }
    if let Some(rate) = voice.get("speech_rate_wpm").and_then(Value::as_u64) {
        attrs.push(format!("rate=\"{rate}\""));
    }
    if let Some(hesitations) = voice.get("hesitation_count").and_then(Value::as_u64)
        && hesitations > 0
    {
        attrs.push(format!("hesitations=\"{hesitations}\""));
    }
    if let Some(energy) = voice.get("energy_band").and_then(Value::as_str)
        && !energy.is_empty()
    {
        attrs.push(format!("energy=\"{}\"", escape_attr(energy)));
    }

    if attrs.is_empty() {
        return None;
    }
    Some(format!(
        "{VOICE_TAG_OPEN}{SENTINEL_ATTR} {} />",
        attrs.join(" ")
    ))
}

/// Join the `audio_events` array labels into a comma-separated, escaped,
/// length-bounded attribute value. `None` when there are no labels.
fn events_attr(voice: &Value) -> Option<String> {
    let events = voice.get("audio_events").and_then(Value::as_array)?;
    let labels: Vec<String> = events
        .iter()
        .filter_map(|event| event.get("label").and_then(Value::as_str))
        .filter(|label| !label.is_empty())
        .map(escape_attr)
        .collect();
    if labels.is_empty() {
        return None;
    }
    let joined = labels.join(",");
    Some(bound_len(&joined, EVENTS_ATTR_CAP))
}

#[derive(Debug, Clone)]
struct IdentityAttr {
    label: String,
    confidence: u64,
    source: String,
    provider_speaker: Option<String>,
}

/// Extract deployment-local identity guesses as escaped, bounded attribute
/// parts. Uses `display` when present, falling back to the stable `id`.
fn speaker_identities_attr(voice: &Value) -> Option<Vec<IdentityAttr>> {
    let identities = voice.get("speaker_identities").and_then(Value::as_array)?;
    let mut out = Vec::new();
    let mut used = 0usize;
    for identity in identities {
        let label = identity_label(identity).map(escape_attr)?;
        let separator = usize::from(!out.is_empty());
        if used + separator + label.len() > SPEAKER_IDENTITIES_ATTR_CAP {
            break;
        }
        used += separator + label.len();
        out.push(IdentityAttr {
            label,
            confidence: identity
                .get("confidence")
                .and_then(Value::as_u64)
                .map_or(0, |value| value.min(100)),
            source: identity
                .get("source")
                .and_then(Value::as_str)
                .filter(|source| !source.is_empty())
                .map_or_else(|| "unknown".to_owned(), escape_attr),
            provider_speaker: identity
                .get("speaker_label")
                .and_then(Value::as_str)
                .filter(|speaker| !speaker.is_empty())
                .map(escape_attr),
        });
    }
    if out.is_empty() { None } else { Some(out) }
}

fn identity_label(identity: &Value) -> Option<&str> {
    identity
        .get("display")
        .and_then(Value::as_str)
        .filter(|display| !display.is_empty())
        .or_else(|| {
            identity
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
        })
}

/// Extract provider speaker labels as escaped, length-bounded attribute parts.
fn speakers_attr(voice: &Value) -> Option<Vec<String>> {
    let speakers = voice.get("speakers").and_then(Value::as_array)?;
    let labels: Vec<String> = speakers
        .iter()
        .filter_map(Value::as_str)
        .filter(|speaker| !speaker.is_empty())
        .map(escape_attr)
        .collect();
    if labels.is_empty() {
        return None;
    }
    let bounded = split_bounded_labels(&labels, SPEAKERS_ATTR_CAP);
    if bounded.is_empty() {
        None
    } else {
        Some(bounded)
    }
}

/// Escape a string for safe inclusion in a double-quoted tag attribute:
/// XML-escape `<`, `>`, `&`, then neutralize the quote that would
/// otherwise close the attribute.
fn escape_attr(s: &str) -> String {
    xml_escape_body(s).replace('"', "&quot;")
}

fn split_bounded_labels(labels: &[String], cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for label in labels {
        let separator = usize::from(!out.is_empty());
        if used + separator + label.len() > cap {
            break;
        }
        used += separator + label.len();
        out.push(label.clone());
    }
    out
}

/// Truncate `s` to at most `cap` bytes without splitting a UTF-8 code
/// point. Escaping runs before this, so the cap never lands inside an
/// entity reference in a way that changes its meaning beyond truncation.
fn bound_len(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_owned();
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("").to_owned()
}

/// Remove a single hook-injected `<voice crabgent="1" ...>` tag plus one
/// following newline from the start of `text`, so re-running the hook on
/// its own output produces exactly one tag.
///
/// The strip is sentinel-anchored: it only fires when the text (after
/// optional leading whitespace) starts with [`INJECTED_TAG_PREFIX`].
/// A user-authored leading `<voice ...>` WITHOUT the private sentinel is
/// attacker-influenced STT content and is returned verbatim, never
/// deleted. A `<voice ...>` elsewhere in the body is likewise untouched.
pub fn strip_prior_voice_tag(text: &str) -> String {
    let leading_ws = text.len() - text.trim_start().len();
    let Some(after_ws) = text.get(leading_ws..) else {
        return text.to_owned();
    };
    let Some(rest) = after_ws.strip_prefix(INJECTED_TAG_PREFIX) else {
        return text.to_owned();
    };
    let Some(close_idx) = rest.find('>') else {
        return text.to_owned();
    };
    // `close_idx` is the byte offset of '>' within `rest`; skip past it.
    let after_tag = rest.get(close_idx + 1..).unwrap_or("");
    after_tag.strip_prefix('\n').unwrap_or(after_tag).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    #[test]
    fn renders_events_pause_and_rate_omits_zero_hesitations() {
        let voice = json!({
            "audio_events": [{"label": "laughter"}],
            "pause_ms": 1200,
            "speech_rate_wpm": 95,
            "hesitation_count": 0,
        });
        let tag = render_voice_tag(&voice).expect("tag");
        assert!(
            tag.starts_with(INJECTED_TAG_PREFIX),
            "sentinel prefix: {tag}"
        );
        assert!(tag.ends_with("/>"), "self-closing: {tag}");
        assert!(tag.contains("events=\"laughter\""));
        assert!(tag.contains("pause_ms=\"1200\""));
        assert!(tag.contains("rate=\"95\""));
        assert!(!tag.contains("hesitations="), "zero hesitations omitted");
    }

    #[test]
    fn renders_all_attributes_present() {
        // Exact rendered shape: sentinel first, then code-order attributes.
        let voice = json!({
            "audio_events": [{"label": "laughter"}],
            "speakers": ["speaker_0"],
            "pause_ms": 1200,
            "speech_rate_wpm": 95,
            "hesitation_count": 2,
            "energy_band": "medium",
        });
        let tag = render_voice_tag(&voice).expect("tag");
        assert_eq!(
            tag,
            "<voice crabgent=\"1\" events=\"laughter\" speaker=\"speaker_0\" pause_ms=\"1200\" rate=\"95\" hesitations=\"2\" energy=\"medium\" />"
        );
    }

    #[test]
    fn renders_multiple_speakers() {
        let voice = json!({"speakers": ["speaker_0", "speaker_1"]});
        let tag = render_voice_tag(&voice).expect("tag");
        assert_eq!(
            tag,
            "<voice crabgent=\"1\" speakers=\"speaker_0,speaker_1\" />"
        );
    }

    #[test]
    fn renders_speaker_identity() {
        let voice = json!({
            "speaker_identities": [{
                "id": "speaker_a",
                "display": "Speaker A",
                "confidence": 87,
                "source": "voiceprint",
                "speaker_label": "speaker_0"
            }]
        });
        let tag = render_voice_tag(&voice).expect("tag");
        assert_eq!(
            tag,
            "<voice crabgent=\"1\" identified_speaker=\"Speaker A\" speaker_confidence=\"87\" speaker_source=\"voiceprint\" speaker_label=\"speaker_0\" />"
        );
    }

    #[test]
    fn renders_multiple_speaker_identities_without_confidence_detail() {
        let voice = json!({
            "speaker_identities": [
                {"id": "speaker_a", "confidence": 87, "source": "voiceprint"},
                {"id": "speaker_b", "display": "Speaker B", "confidence": 79, "source": "claimed"}
            ]
        });
        let tag = render_voice_tag(&voice).expect("tag");
        assert_eq!(
            tag,
            "<voice crabgent=\"1\" identified_speakers=\"speaker_a,Speaker B\" />"
        );
    }

    #[test]
    fn omitted_fields_are_not_rendered() {
        // A `voice` object with only events: pause/rate/hesitations/energy
        // are absent (the compute layer encoded them as `None`), so none
        // of those attributes appear. The renderer never re-gates this.
        let voice = json!({"audio_events": [{"label": "sigh"}]});
        let tag = render_voice_tag(&voice).expect("tag");
        assert!(tag.starts_with(INJECTED_TAG_PREFIX), "sentinel: {tag}");
        assert!(tag.contains("events=\"sigh\""));
        assert!(!tag.contains("pause_ms="));
        assert!(!tag.contains("rate="));
        assert!(!tag.contains("hesitations="));
        assert!(!tag.contains("energy="));
    }

    #[test]
    fn empty_voice_object_renders_nothing() {
        assert_eq!(render_voice_tag(&json!({})), None);
    }

    #[test]
    fn energy_band_renders_when_present() {
        let voice = json!({"audio_events": [{"label": "applause"}], "energy_band": "high"});
        let tag = render_voice_tag(&voice).expect("tag");
        assert!(tag.contains("energy=\"high\""), "energy: {tag}");
    }

    fn attr_value<'a>(tag: &'a str, attr: &str) -> &'a str {
        let open = format!("{attr}=\"");
        tag.split_once(&open)
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(value, _)| value)
            .expect("attribute present")
    }

    #[test]
    fn hostile_label_is_attribute_escaped() {
        let voice = json!({"audio_events": [{"label": "\"><script>"}]});
        let tag = render_voice_tag(&voice).expect("tag");
        let value = attr_value(&tag, "events");
        assert!(!value.contains('<'), "no raw < in {value}");
        assert!(value.contains("&quot;"), "quote escaped in {value}");
        assert!(value.contains("&lt;"), "lt escaped in {value}");
    }

    #[test]
    fn empty_labels_are_skipped() {
        let voice = json!({"audio_events": [{"label": ""}, {"no_label": 1}]});
        assert_eq!(render_voice_tag(&voice), None);
    }

    #[test]
    fn events_value_is_length_bounded() {
        let many: Vec<Value> = (0..100).map(|_| json!({"label": "abcdefgh"})).collect();
        let voice = json!({"audio_events": many});
        let tag = render_voice_tag(&voice).expect("tag");
        let value = attr_value(&tag, "events");
        assert!(value.len() <= EVENTS_ATTR_CAP, "bounded: {}", value.len());
    }

    #[test]
    fn speakers_value_is_length_bounded() {
        let many: Vec<Value> = (0..100).map(|i| json!(format!("speaker_{i}"))).collect();
        let voice = json!({"speakers": many});
        let tag = render_voice_tag(&voice).expect("tag");
        let value = attr_value(&tag, "speakers");
        assert!(value.len() <= SPEAKERS_ATTR_CAP, "bounded: {}", value.len());
    }

    #[test]
    fn strip_prior_voice_tag_removes_leading_injected_tag() {
        // Only a sentinel-bearing leading tag is stripped.
        let input = "<voice crabgent=\"1\" events=\"x\" />\nbody";
        assert_eq!(
            strip_prior_voice_tag(input),
            "body",
            "injected tag stripped"
        );
    }

    #[test]
    fn strip_prior_voice_tag_keeps_leading_user_tag_without_sentinel() {
        // A user-authored leading `<voice ...>` lacks the private
        // sentinel: it is attacker-influenced STT content and must be
        // preserved verbatim, never deleted.
        let calm = "<voice events=\"calm\" />\nI am calm";
        assert_eq!(strip_prior_voice_tag(calm), calm, "user tag preserved");

        let faked = "<voice fake=\"1\">hello";
        assert_eq!(
            strip_prior_voice_tag(faked),
            faked,
            "faked sentinel-ish tag preserved"
        );
    }

    #[test]
    fn strip_prior_voice_tag_ignores_mid_body_tag() {
        // A `<voice ...>` not at the very start is left untouched.
        let input = "real body with <voice fake> in the middle";
        assert_eq!(
            strip_prior_voice_tag(input),
            input,
            "mid-body tag preserved"
        );
    }

    #[test]
    fn strip_prior_voice_tag_keeps_text_without_tag() {
        assert_eq!(strip_prior_voice_tag("no tag here"), "no tag here");
    }

    #[test]
    fn bound_len_truncates_on_char_boundary() {
        let long = "ä".repeat(200); // 2 bytes each = 400 bytes.
        let bounded = bound_len(&long, EVENTS_ATTR_CAP);
        assert!(bounded.len() <= EVENTS_ATTR_CAP);
        assert!(
            std::str::from_utf8(bounded.as_bytes()).is_ok(),
            "valid utf-8 after truncation"
        );
    }
}
