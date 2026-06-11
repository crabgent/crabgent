use serde_json::Value;

pub(super) fn value_to_string(value: &Value) -> String {
    value.as_str().map_or_else(
        || serde_json::to_string(value).unwrap_or_default(),
        str::to_owned,
    )
}
