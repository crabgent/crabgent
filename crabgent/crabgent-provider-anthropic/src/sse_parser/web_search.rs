//! Handlers for Anthropic web-search SSE blocks.
//!
//! `server_tool_use` blocks accumulate the search query; the tool-result
//! block carries the `encrypted_content` items and optional text blocks
//! with citations.  A single `ProviderEvent::ServerToolResult` is emitted
//! on `content_block_stop` of the `web_search_tool_result` block.

use crabgent_core::ProviderEvent;
use crabgent_core::types::Citation;
use serde_json::{Value, json};

/// Accumulated state for a `web_search_tool_result` block.
#[derive(Debug)]
pub(super) struct WebSearchResultBuilder {
    /// Full block JSON accumulated from the raw SSE `content_block` value.
    pub(super) block: Value,
}

impl WebSearchResultBuilder {
    pub(super) const fn new(block: Value) -> Self {
        Self { block }
    }

    /// Finalise the block, extracting citations from any nested text blocks.
    pub(super) fn finalize(self) -> ProviderEvent {
        let citations = extract_citations(&self.block);
        ProviderEvent::ServerToolResult {
            provider: "anthropic".into(),
            name: "web_search".into(),
            content: self.block,
            citations,
        }
    }
}

/// Walk the `content` array of a `web_search_tool_result` block and pull out
/// any `web_search_result_location` citations from nested text blocks.
pub(super) fn extract_citations(block: &Value) -> Vec<Citation> {
    let Some(content) = block.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in content {
        if item.get("type").and_then(Value::as_str) == Some("text")
            && let Some(cits) = item.get("citations").and_then(Value::as_array)
        {
            for c in cits {
                if c.get("type").and_then(Value::as_str) == Some("web_search_result_location")
                    && let Some(citation) = parse_citation(c)
                {
                    out.push(citation);
                }
            }
        }
    }
    out
}

fn parse_citation(raw: &Value) -> Option<Citation> {
    let url = raw.get("url").and_then(Value::as_str)?.to_string();
    let title = raw
        .get("title")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let cited_text = raw
        .get("cited_text")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Some(Citation {
        url,
        title,
        cited_text,
        provider: "anthropic".into(),
        raw: raw.clone(),
    })
}

/// Convert a `web_search_tool_result` `content_block_start` payload into a
/// `WebSearchResultBuilder`.  The full `content_block` JSON is stored
/// verbatim so `encrypted_content` is preserved for multi-turn echoing.
pub(super) fn web_search_result_builder(block: &Value) -> WebSearchResultBuilder {
    // If the block has no `content` array yet (rare: Anthropic sends the full
    // block in content_block_start), still preserve it verbatim.
    let stored = if block.get("content").is_some() {
        block.clone()
    } else {
        // Synthesize a minimal shell; real content will never arrive via delta
        // for server-result blocks, so this is just a safe fallback.
        let mut shell = block.clone();
        if let Some(obj) = shell.as_object_mut() {
            obj.entry("content").or_insert_with(|| json!([]));
        }
        shell
    };
    WebSearchResultBuilder::new(stored)
}
