use crate::{JsonRpcResponse, McpError};

pub(super) fn parse_rpc_response(body: &str, is_sse: bool) -> Result<JsonRpcResponse, McpError> {
    if is_sse {
        parse_sse_response(body)
    } else {
        serde_json::from_str::<JsonRpcResponse>(body)
            .map_err(|err| McpError::Decode(err.to_string()))
    }
}

fn parse_sse_response(body: &str) -> Result<JsonRpcResponse, McpError> {
    let body = body.replace("\r\n", "\n");
    let events = body.split("\n\n").collect::<Vec<_>>();
    let Some(event) = events.iter().rev().find(|event| {
        event
            .lines()
            .any(|line| line.trim_start().starts_with("data:"))
    }) else {
        return Err(McpError::Decode(
            "SSE response missing data event".to_string(),
        ));
    };

    let data = event
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");

    serde_json::from_str(&data).map_err(|err| McpError::Decode(err.to_string()))
}
