use serde_json::{Value, json};

pub const MIN_MESSAGE_CACHE_BREAKPOINT_BYTES: usize = 3 * 1024;
const HAIKU_MIN_BYTES: usize = 16 * 1024;

pub fn wrap_system(system: Option<&str>, ttl: Option<&str>) -> Value {
    match (system, ttl) {
        (Some(text), Some(ttl)) => json!([{
            "type": "text",
            "text": text,
            "cache_control": cache_control(ttl),
        }]),
        (Some(text), None) => Value::String(text.to_string()),
        _ => Value::Null,
    }
}

pub fn apply_tool_cache(tools: &mut [Value], ttl: Option<&str>) {
    let Some(ttl) = ttl else {
        return;
    };
    let Some(tool) = tools.last_mut().and_then(Value::as_object_mut) else {
        return;
    };

    tool.insert("cache_control".to_string(), cache_control(ttl));
}

pub fn apply_message_cache(messages: &mut [Value], ttl: Option<&str>, model_id: &str) {
    let Some(ttl) = ttl else {
        return;
    };
    let Some(last_msg) = messages.last_mut() else {
        return;
    };

    normalize_string_content(last_msg);

    if !message_reaches_cache_threshold(last_msg, model_min_bytes(model_id)) {
        return;
    }
    let Some(content) = last_msg.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    let Some(last_block) = content.last_mut().and_then(Value::as_object_mut) else {
        return;
    };

    last_block.insert("cache_control".to_string(), cache_control(ttl));
}

fn normalize_string_content(msg: &mut Value) {
    let Some(content) = msg.get_mut("content") else {
        return;
    };
    let Some(text) = content.as_str().map(str::to_string) else {
        return;
    };

    *content = json!([{"type": "text", "text": text}]);
}

fn model_min_bytes(model_id: &str) -> usize {
    if model_id.to_lowercase().contains("haiku") {
        HAIKU_MIN_BYTES
    } else {
        MIN_MESSAGE_CACHE_BREAKPOINT_BYTES
    }
}

fn message_reaches_cache_threshold(msg: &Value, min_bytes: usize) -> bool {
    estimated_message_content_bytes(msg).is_some_and(|bytes| bytes >= min_bytes)
}

fn estimated_message_content_bytes(msg: &Value) -> Option<usize> {
    let content = msg.get("content")?;
    serde_json::to_vec(content).ok().map(|bytes| bytes.len())
}

fn cache_control(ttl: &str) -> Value {
    json!({"type": "ephemeral", "ttl": ttl})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_system_none_system_returns_null() {
        assert!(wrap_system(None, None).is_null());
        assert!(wrap_system(None, Some("5m")).is_null());
    }

    #[test]
    fn wrap_system_no_ttl_returns_string() {
        let system = wrap_system(Some("be terse"), None);

        assert_eq!(system, "be terse");
    }

    #[test]
    fn wrap_system_with_ttl_returns_array_with_cache_control() {
        let system = wrap_system(Some("be terse"), Some("5m"));

        assert_eq!(system[0]["type"], "text");
        assert_eq!(system[0]["text"], "be terse");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(system[0]["cache_control"]["ttl"], "5m");
    }

    #[test]
    fn apply_tool_cache_sets_on_last_tool() {
        let mut tools = vec![json!({"name": "first"}), json!({"name": "last"})];

        apply_tool_cache(&mut tools, Some("1h"));

        assert!(tools[0].get("cache_control").is_none());
        assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(tools[1]["cache_control"]["ttl"], "1h");
    }

    #[test]
    fn apply_tool_cache_noop_when_ttl_none() {
        let mut tools = vec![json!({"name": "tool"})];

        apply_tool_cache(&mut tools, None);

        assert!(tools[0].get("cache_control").is_none());
    }

    #[test]
    fn apply_tool_cache_noop_when_empty() {
        let mut tools = Vec::new();

        apply_tool_cache(&mut tools, Some("5m"));

        assert!(tools.is_empty());
    }

    #[test]
    fn apply_message_cache_skips_when_under_min_bytes() {
        let mut messages =
            vec![json!({"role": "user", "content": [{"type": "text", "text": "short"}]})];

        apply_message_cache(&mut messages, Some("5m"), "claude-sonnet-4-6");

        assert!(messages[0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn apply_message_cache_sets_on_last_block_when_large() {
        let large_text = "x".repeat(MIN_MESSAGE_CACHE_BREAKPOINT_BYTES);
        let mut messages = vec![json!({
            "role": "user",
            "content": [
                {"type": "text", "text": large_text},
                {"type": "text", "text": "tail"}
            ]
        })];

        apply_message_cache(&mut messages, Some("5m"), "claude-sonnet-4-6");

        assert!(messages[0]["content"][0].get("cache_control").is_none());
        assert_eq!(
            messages[0]["content"][1]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(messages[0]["content"][1]["cache_control"]["ttl"], "5m");
    }

    #[test]
    fn apply_message_cache_transforms_string_content_to_array() {
        let mut messages = vec![json!({"role": "user", "content": "hello"})];

        apply_message_cache(&mut messages, Some("5m"), "claude-sonnet-4-6");

        assert_eq!(messages[0]["content"][0]["type"], "text");
        assert_eq!(messages[0]["content"][0]["text"], "hello");
        assert!(messages[0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn apply_message_cache_noop_when_ttl_none() {
        let original = json!({"role": "user", "content": "hello"});
        let mut messages = vec![original.clone()];

        apply_message_cache(&mut messages, None, "claude-sonnet-4-6");

        assert_eq!(messages[0], original);
    }

    #[test]
    fn haiku_below_min_skips_cache() {
        let large_text = "x".repeat(12 * 1024);
        let mut messages = vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": large_text}]
        })];

        apply_message_cache(&mut messages, Some("5m"), "claude-haiku-4-5");

        assert!(messages[0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn haiku_above_min_inserts_cache() {
        let large_text = "x".repeat(20 * 1024);
        let mut messages = vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": large_text}]
        })];

        apply_message_cache(&mut messages, Some("5m"), "claude-haiku-4-5");

        assert_eq!(
            messages[0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }

    #[test]
    fn sonnet_at_3kb_threshold_inserts_cache() {
        let large_text = "x".repeat(4 * 1024);
        let mut messages = vec![json!({
            "role": "user",
            "content": [{"type": "text", "text": large_text}]
        })];

        apply_message_cache(&mut messages, Some("5m"), "claude-sonnet-4-6");

        assert_eq!(
            messages[0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
    }
}
