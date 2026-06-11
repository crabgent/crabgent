//! Web-search (Google grounding) integration tests.

use std::time::Duration;

use crabgent_core::{
    LlmRequest, Provider, ProviderEvent, RunCtx, RunId, Subject, ToolDef, types::WebSearchConfig,
};
use crabgent_provider_google::models::GEMINI_3_5_FLASH;
use crabgent_provider_google::{GoogleConfig, GoogleProvider};
use futures::StreamExt;
use serde_json::{Value, json};

const API_KEY_SECRET: &str = "secret-test-google-key-99999";

fn config(base_url: &str) -> GoogleConfig {
    GoogleConfig::new(API_KEY_SECRET.to_owned())
        .with_base_url(base_url.to_owned())
        .with_max_retries(0)
        .with_retry_base_delay(Duration::from_millis(1))
        .with_request_timeout(Duration::from_secs(2))
}

fn ctx() -> RunCtx {
    RunCtx::new(RunId::new(), Subject::new("test-subject"))
}

const MINIMAL_RESPONSE: &str = r#"{
    "modelVersion": "gemini-3.5-flash",
    "candidates": [{"finishReason": "STOP", "content": {"parts": [{"text": "ok"}]}}]
}"#;

fn web_search_enabled() -> WebSearchConfig {
    WebSearchConfig {
        enabled: true,
        ..WebSearchConfig::default()
    }
}

fn base_req() -> LlmRequest {
    LlmRequest {
        model: GEMINI_3_5_FLASH.into(),
        system_prompt: None,
        messages: vec![json!({"role": "user", "content": "hello"})],
        tools: Vec::new(),
        max_tokens: None,
        temperature: None,
        stop_sequences: Vec::new(),
        reasoning_effort: None,
        web_search: WebSearchConfig::default(),
        tool_choice: None,
    }
}

fn one_tool() -> ToolDef {
    ToolDef {
        name: "lookup".to_owned(),
        description: "look up one thing".to_owned(),
        input_schema: json!({"type": "object"}),
    }
}

fn body_json(request: &mockito::Request) -> Option<Value> {
    let body = request.utf8_lossy_body().ok()?;
    serde_json::from_str::<Value>(&body).ok()
}

// --- tools_to_google wire tests ---

#[tokio::test]
async fn tools_to_google_emits_function_declarations_without_web_search() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .match_request(|req: &mockito::Request| {
            let Some(body) = body_json(req) else {
                return false;
            };
            let tools = body["tools"].as_array();
            tools.is_some_and(|t| {
                t.len() == 1
                    && t[0]["functionDeclarations"][0]["name"] == "lookup"
                    && t[0].get("google_search").is_none()
            }) && body.get("toolConfig").is_none()
        })
        .with_status(200)
        .with_body(MINIMAL_RESPONSE)
        .expect(1)
        .create_async()
        .await;

    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");
    let req = LlmRequest {
        tools: vec![one_tool()],
        web_search: WebSearchConfig::default(), // disabled
        ..base_req()
    };
    provider.complete(&req, &ctx(), None).await.expect("ok");
    mock.assert_async().await;
}

#[tokio::test]
async fn tools_to_google_emits_only_google_search_when_no_user_tools() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .match_request(|req: &mockito::Request| {
            let Some(body) = body_json(req) else {
                return false;
            };
            let tools = body["tools"].as_array();
            tools.is_some_and(|t| {
                t.len() == 1
                    && t[0].get("google_search").is_some()
                    && t[0].get("functionDeclarations").is_none()
            }) && body.get("toolConfig").is_none()
        })
        .with_status(200)
        .with_body(MINIMAL_RESPONSE)
        .expect(1)
        .create_async()
        .await;

    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");
    let req = LlmRequest {
        tools: Vec::new(),
        web_search: web_search_enabled(),
        ..base_req()
    };
    provider.complete(&req, &ctx(), None).await.expect("ok");
    mock.assert_async().await;
}

#[tokio::test]
async fn tools_to_google_emits_both_when_user_tools_and_web_search() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/v1beta/models/gemini-3.5-flash:generateContent")
        .match_request(|req: &mockito::Request| {
            let Some(body) = body_json(req) else {
                return false;
            };
            let tools = body["tools"].as_array();
            tools.is_some_and(|t| {
                t.len() == 2
                    && t[0]["functionDeclarations"][0]["name"] == "lookup"
                    && t[1].get("google_search").is_some()
            }) && body["toolConfig"]["includeServerSideToolInvocations"] == true
        })
        .with_status(200)
        .with_body(MINIMAL_RESPONSE)
        .expect(1)
        .create_async()
        .await;

    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");
    let req = LlmRequest {
        tools: vec![one_tool()],
        web_search: web_search_enabled(),
        ..base_req()
    };
    provider.complete(&req, &ctx(), None).await.expect("ok");
    mock.assert_async().await;
}

// --- groundingMetadata parse test ---

#[tokio::test]
async fn grounding_metadata_emits_server_tool_result_with_citations() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock(
            "POST",
            "/v1beta/models/gemini-3.5-flash:streamGenerateContent",
        )
        .match_query(mockito::Matcher::UrlEncoded("alt".into(), "sse".into()))
        .with_status(200)
        .with_header("content-type", "text/event-stream")
        .with_body(concat!(
            "data: {\"modelVersion\":\"gemini-3.5-flash\",",
            "\"candidates\":[{\"finishReason\":\"STOP\",",
            "\"content\":{\"parts\":[{\"text\":\"grounded answer\"}]},",
            "\"groundingMetadata\":{\"groundingChunks\":[",
            "{\"web\":{\"uri\":\"https://example.com/page\",\"title\":\"Example Page\"}},",
            "{\"web\":{\"uri\":\"https://other.com/\",\"title\":\"Other\"}}",
            "],\"groundingSupports\":[{\"segment\":{\"startIndex\":0,\"endIndex\":15}}],",
            "\"webSearchQueries\":[\"example query\"]}}]}\n\n",
        ))
        .expect(1)
        .create_async()
        .await;

    let provider = GoogleProvider::try_new(reqwest::Client::new(), config(&server.url()))
        .expect("valid provider");
    let req = LlmRequest {
        web_search: web_search_enabled(),
        ..base_req()
    };
    let mut event_stream = provider
        .stream(&req, &ctx(), None)
        .await
        .expect("stream ok");

    let mut server_tool_result = None;
    while let Some(event) = event_stream.next().await {
        if let Ok(ProviderEvent::ServerToolResult {
            provider: prov,
            name,
            content,
            citations,
        }) = event
        {
            server_tool_result = Some((prov, name, content, citations));
        }
    }

    mock.assert_async().await;

    let (prov, name, content, citations) =
        server_tool_result.expect("ServerToolResult event missing");
    assert_eq!(prov, "google");
    assert_eq!(name, "google_search");

    // content carries the full groundingMetadata JSON
    assert!(content["groundingChunks"].is_array());
    assert!(
        content["webSearchQueries"]
            .as_array()
            .is_some_and(|q| !q.is_empty())
    );

    assert_eq!(citations.len(), 2);
    assert_eq!(citations[0].url, "https://example.com/page");
    assert_eq!(citations[0].title.as_deref(), Some("Example Page"));
    assert_eq!(citations[0].provider, "google");
    assert!(citations[0].cited_text.is_none());
    assert_eq!(citations[1].url, "https://other.com/");
}
