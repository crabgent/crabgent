//! Test-only helpers for building Telegram update JSON.
//!
//! Lives outside `poller.rs` to keep that module under the LOC cap.
//! Tagged `pub` (not `pub(crate)`) so external integration tests
//! reuse the helpers via `crate::poller::test_helpers::*`.

use serde_json::{Value, json};

/// Build a `TelegramUpdate`-shaped JSON payload from a partial
/// message description.
#[must_use]
pub fn build_update_json(
    update_id: i64,
    chat_id: i64,
    user_id: i64,
    text: &str,
    chat_type: &str,
) -> Value {
    json!({
        "update_id": update_id,
        "message": {
            "message_id": update_id,
            "date": 1_700_000_000,
            "chat": {"id": chat_id, "type": chat_type},
            "from": {"id": user_id, "username": format!("u{user_id}")},
            "text": text,
        }
    })
}

/// Build a `TelegramUpdate`-shaped JSON payload carrying a
/// `message_reaction` update. `old_emojis` and `new_emojis` are
/// raw emoji strings; each turns into a `{"type": "emoji", ...}`
/// entry inside the corresponding reaction array.
#[must_use]
pub fn build_reaction_update_json(
    update_id: i64,
    chat_id: i64,
    user_id: i64,
    message_id: i64,
    chat_type: &str,
    old_emojis: &[&str],
    new_emojis: &[&str],
) -> Value {
    let map_emojis = |xs: &[&str]| -> Vec<Value> {
        xs.iter()
            .map(|e| json!({"type": "emoji", "emoji": e}))
            .collect()
    };
    json!({
        "update_id": update_id,
        "message_reaction": {
            "chat": {"id": chat_id, "type": chat_type},
            "message_id": message_id,
            "user": {"id": user_id, "username": format!("u{user_id}")},
            "date": 1_700_000_000,
            "old_reaction": map_emojis(old_emojis),
            "new_reaction": map_emojis(new_emojis),
        }
    })
}
