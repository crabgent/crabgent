use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct McpToolList {
    pub tools: Vec<McpToolDef>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct McpToolDef {
    // Initial: bare String, Newtype pending
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpCallResult {
    pub content: Value,
    pub is_error: bool,
}
