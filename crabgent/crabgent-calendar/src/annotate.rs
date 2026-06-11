//! Inline-rewrite the most recent user messages on the wire so the
//! LLM sees `[YYYY-MM-DD HH:MM ZONE, Xh ago]` before the body text.
//!
//! Operates on the JSON-Value `messages` field of `LlmRequest`. The
//! original session-store `Vec<Message>` is untouched: only the
//! per-call wire clone produced by `Decision::Replace` carries these
//! prefixes, so each turn rebuilds annotations from scratch.

use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use crate::hint_format::format_duration;

/// Prepend `[ts, ago]` to the first `ContentBlock::Text` of up to
/// `limit` most recent user messages that carry a parseable
/// `timestamp` field. Messages without a text block (image- or
/// audio-only) are skipped silently so the wire stays well-formed.
pub fn annotate_recent_user_messages(
    messages: &mut [Value],
    now: DateTime<Utc>,
    tz: Tz,
    limit: usize,
) {
    if limit == 0 {
        return;
    }
    let mut annotated = 0_usize;
    for msg in messages.iter_mut().rev() {
        if annotated >= limit {
            break;
        }
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let Some(ts_str) = msg.get("timestamp").and_then(Value::as_str) else {
            continue;
        };
        let Ok(parsed) = DateTime::parse_from_rfc3339(ts_str) else {
            continue;
        };
        let ts = parsed.with_timezone(&Utc);
        let prefix = format_inline_prefix(ts, now, tz);
        if prepend_to_first_text(msg, &prefix) {
            annotated += 1;
        }
    }
}

fn format_inline_prefix(ts: DateTime<Utc>, now: DateTime<Utc>, tz: Tz) -> String {
    let local = ts.with_timezone(&tz);
    let delta = now.signed_duration_since(ts);
    let delta = if delta < Duration::zero() {
        Duration::zero()
    } else {
        delta
    };
    format!(
        "[{} {}, {}]\n",
        local.format("%Y-%m-%d %H:%M"),
        tz.name(),
        format_duration(delta),
    )
}

/// Insert `prefix` at the beginning of the first text block in the
/// message's `content` array. Returns `true` when a text block was
/// found and rewritten.
fn prepend_to_first_text(msg: &mut Value, prefix: &str) -> bool {
    let Some(content) = msg.get_mut("content").and_then(Value::as_array_mut) else {
        return false;
    };
    for block in content.iter_mut() {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(existing) = block.get("text").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        let updated = format!("{prefix}{existing}");
        if let Some(obj) = block.as_object_mut() {
            obj.insert("text".to_owned(), Value::String(updated));
            return true;
        }
        return false;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-21T14:32:11Z")
            .expect("rfc3339")
            .with_timezone(&Utc)
    }

    fn user_msg(text: &str, ts: Option<&str>) -> Value {
        let mut obj = json!({
            "role": "user",
            "content": [{"type": "text", "text": text}],
        });
        if let Some(t) = ts {
            obj.as_object_mut()
                .expect("obj")
                .insert("timestamp".to_owned(), Value::String(t.to_owned()));
        }
        obj
    }

    #[test]
    fn annotates_only_the_last_n_user_messages() {
        let berlin: Tz = "Europe/Berlin".parse().expect("tz");
        let mut messages = vec![
            user_msg("msg1", Some("2026-05-18T14:32:11Z")),
            user_msg("msg2", Some("2026-05-19T14:32:11Z")),
            user_msg("msg3", Some("2026-05-20T14:32:11Z")),
            user_msg("msg4", Some("2026-05-21T10:00:00Z")),
            user_msg("msg5", Some("2026-05-21T14:30:00Z")),
        ];
        annotate_recent_user_messages(&mut messages, now(), berlin, 3);
        let text_of = |i: usize| {
            messages[i]["content"][0]["text"]
                .as_str()
                .expect("text")
                .to_owned()
        };
        assert_eq!(text_of(0), "msg1");
        assert_eq!(text_of(1), "msg2");
        assert!(text_of(2).starts_with("[2026-05-20 16:32 Europe/Berlin"));
        assert!(text_of(3).starts_with("[2026-05-21 12:00 Europe/Berlin"));
        assert!(text_of(4).starts_with("[2026-05-21 16:30 Europe/Berlin"));
    }

    #[test]
    fn idempotent_when_rebuilt_from_pristine_input() {
        let berlin: Tz = "Europe/Berlin".parse().expect("tz");
        let pristine = vec![user_msg("msg1", Some("2026-05-21T10:00:00Z"))];

        let mut first = pristine.clone();
        annotate_recent_user_messages(&mut first, now(), berlin, 5);

        let mut second = pristine;
        annotate_recent_user_messages(&mut second, now(), berlin, 5);

        assert_eq!(first, second);
    }

    #[test]
    fn skips_user_messages_without_timestamp() {
        let berlin: Tz = "Europe/Berlin".parse().expect("tz");
        let mut messages = vec![user_msg("bare", None)];
        annotate_recent_user_messages(&mut messages, now(), berlin, 5);
        assert_eq!(messages[0]["content"][0]["text"].as_str(), Some("bare"));
    }

    #[test]
    fn skips_messages_without_text_block() {
        let berlin: Tz = "Europe/Berlin".parse().expect("tz");
        let image_only = json!({
            "role": "user",
            "content": [{"type": "image", "mime": "image/png", "data": "AAAA"}],
            "timestamp": "2026-05-21T10:00:00Z",
        });
        let mut messages = vec![image_only.clone()];
        annotate_recent_user_messages(&mut messages, now(), berlin, 5);
        assert_eq!(messages[0], image_only);
    }

    #[test]
    fn ignores_assistant_messages() {
        let berlin: Tz = "Europe/Berlin".parse().expect("tz");
        let assistant = json!({
            "role": "assistant",
            "text": "hi",
            "tool_calls": [],
            "timestamp": "2026-05-21T10:00:00Z",
        });
        let mut messages = vec![assistant.clone()];
        annotate_recent_user_messages(&mut messages, now(), berlin, 5);
        assert_eq!(messages[0], assistant);
    }
}
