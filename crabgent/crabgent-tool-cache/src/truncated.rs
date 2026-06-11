use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TruncatedOutput {
    pub cached: bool,
    pub cache_id: String,
    pub size_tokens: usize,
    pub preview: String,
    pub hint: String,
}

impl TruncatedOutput {
    pub fn new(cache_id: String, size_tokens: usize, preview: String) -> Self {
        let hint = hint_for_cache_id(&cache_id);
        Self {
            cached: true,
            cache_id,
            size_tokens,
            preview,
            hint,
        }
    }
}

pub fn hint_for_cache_id(cache_id: &str) -> String {
    format!(
        "Cache retrieval strategies:\n\
         1. cache_read: call tool 'cache_read' with {{\"id\":\"{cache_id}\"}} to retrieve the full content.\n\
         2. task: spawn a background task with {{\"op\":\"create\",\"prompt\":\"process the cached content\",\"context\":{{\"cache_ids\":[\"{cache_id}\"]}}}} for async processing without blocking context."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truncated_json_shape_deny_unknown_fields() {
        let value = json!({
            "cached": true,
            "cache_id": "cache-1",
            "size_tokens": 42,
            "preview": "abcd",
            "hint": "use cache_read",
            "extra": "blocked"
        });

        serde_json::from_value::<TruncatedOutput>(value).expect_err("expected error");
    }

    #[test]
    fn hint_contains_both_cache_read_and_task_spawn() {
        let hint = hint_for_cache_id("cache-2");

        assert!(hint.contains("cache_read"));
        assert!(hint.contains("task"));
    }
}
