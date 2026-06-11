//! Trait-boundary flight recorder.
//!
//! Captures one kernel turn worth of data crossing the host's
//! out-going trait surfaces: the `Hook` chain (`before_llm`,
//! `before_tool`, `after_tool`, `on_stop`), `EmbeddingProvider::embed`,
//! and `ChannelSink` (send / react / edit / delete / upload /
//! `notify_user`). Each captured event is appended to
//! `<dir>/<run_id>.jsonl`. One file per kernel run.
//!
//! Activation: set `CRABGENT_FLIGHT_DIR=<path>` in the environment.
//! Unset = recorder disabled.
//!
//! Coverage limits
//! - LLM RESPONSE bodies are not directly captured (the `Hook` trait
//!   exposes `before_llm` but no `after_llm`; capturing response
//!   payloads would require wrapping the `Provider` trait, which is
//!   sync/stream-shaped and out of scope here). Tool calls + final
//!   `Outcome.text` already carry the assistant-visible side-effects.
//! - Raw HTTPS bytes are not captured. The trait-boundary payload is
//!   semantically equivalent to what reqwest/matrix-sdk serialise on
//!   the wire next, minus TLS framing.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use crabgent_channel::{
    ChannelError, ChannelSink, MessageRef, OutboundMessage, ParticipantId, ReadMessage,
};
use crabgent_core::{
    Decision, EmbeddingError, EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, Hook,
    LlmRequest, ModelId, Outcome, Owner, RunCtx, RunId, Subject, ToolCall, ToolResult,
};
use crabgent_log::warn;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

const TEXT_PREVIEW_CAP: usize = 4096;

pub struct FlightRecorder {
    dir: PathBuf,
    /// `Subject.id()` -> active `RunId`. Populated in `before_llm` and
    /// cleared in `on_stop`. Sink decorators consult this so calls
    /// outside the Hook chain still get correlated with the run that
    /// triggered them.
    subject_to_run: Arc<Mutex<HashMap<String, RunId>>>,
}

impl FlightRecorder {
    #[must_use]
    pub fn from_env() -> Option<Arc<Self>> {
        let dir = std::env::var("CRABGENT_FLIGHT_DIR").ok()?;
        if dir.trim().is_empty() {
            return None;
        }
        let dir = PathBuf::from(dir);
        if let Err(err) = std::fs::create_dir_all(&dir) {
            warn!(
                dir = %dir.display(),
                error = %err,
                "flight: create_dir_all failed; recorder disabled",
            );
            return None;
        }
        Some(Arc::new(Self {
            dir,
            subject_to_run: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    pub fn hook(self: &Arc<Self>) -> Arc<FlightHook> {
        Arc::new(FlightHook {
            rec: Arc::clone(self),
        })
    }

    pub fn wrap_embedder(
        self: &Arc<Self>,
        inner: Arc<dyn EmbeddingProvider>,
    ) -> Arc<dyn EmbeddingProvider> {
        Arc::new(RecordingEmbedder {
            rec: Arc::clone(self),
            inner,
        })
    }

    pub fn wrap_sink(self: &Arc<Self>, inner: Arc<dyn ChannelSink>) -> Arc<dyn ChannelSink> {
        Arc::new(RecordingSink {
            rec: Arc::clone(self),
            inner,
        })
    }

    async fn write(&self, run_id: &RunId, kind: &str, payload: Value) {
        let line = json!({
            "ts": Utc::now().to_rfc3339(),
            "run_id": run_id.to_string(),
            "kind": kind,
            "payload": payload,
        });
        let path = self.dir.join(format!("{run_id}.jsonl"));
        let line_str = match serde_json::to_string(&line) {
            Ok(s) => s,
            Err(err) => {
                warn!(error = %err, "flight: json serialize failed");
                return;
            }
        };
        let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            use std::fs::OpenOptions;
            let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
            writeln!(f, "{line_str}")?;
            Ok(())
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(err)) => warn!(error = %err, "flight: write failed"),
            Err(err) => warn!(error = %err, "flight: spawn_blocking join failed"),
        }
    }

    async fn run_id_for(&self, subject: &Subject) -> Option<RunId> {
        let map = self.subject_to_run.lock().await;
        map.get(subject.id()).cloned()
    }
}

pub struct FlightHook {
    rec: Arc<FlightRecorder>,
}

#[async_trait]
impl Hook for FlightHook {
    async fn before_llm(&self, req: &LlmRequest, ctx: &RunCtx) -> Decision<LlmRequest> {
        {
            let mut map = self.rec.subject_to_run.lock().await;
            map.insert(ctx.subject.id().to_owned(), ctx.run_id.clone());
        }
        if let Some(tail) = last_user_text(&req.messages) {
            self.rec
                .write(
                    &ctx.run_id,
                    "inbound",
                    json!({
                        "subject_id": ctx.subject.id(),
                        "tail_user": truncate(&tail, TEXT_PREVIEW_CAP),
                    }),
                )
                .await;
        }
        let payload = redact_large_inline_bytes(serde_json::to_value(req).unwrap_or(Value::Null));
        self.rec.write(&ctx.run_id, "llm_request", payload).await;
        Decision::Continue
    }

    async fn before_tool(&self, call: &ToolCall, ctx: &RunCtx) -> Decision<ToolCall> {
        self.rec
            .write(
                &ctx.run_id,
                "tool_call",
                json!({
                    "name": call.name,
                    "id": call.id,
                    "args": redact_large_inline_bytes(call.args.clone()),
                }),
            )
            .await;
        Decision::Continue
    }

    async fn after_tool(
        &self,
        call: &ToolCall,
        result: &ToolResult,
        ctx: &RunCtx,
    ) -> Decision<ToolResult> {
        self.rec
            .write(
                &ctx.run_id,
                "tool_result",
                json!({
                    "name": call.name,
                    "id": call.id,
                    "is_error": result.is_error,
                    "output": redact_large_inline_bytes(result.output.clone()),
                }),
            )
            .await;
        Decision::Continue
    }

    async fn on_stop(&self, ctx: &RunCtx, outcome: &Outcome) {
        let (kind, text) = match outcome {
            Outcome::Completed(t) => ("completed", t.clone()),
            Outcome::Errored(err) => ("errored", err.clone()),
            Outcome::Cancelled => ("cancelled", String::new()),
            Outcome::MaxTurnsExceeded => ("max_turns_exceeded", String::new()),
            _ => ("unknown", String::new()),
        };
        self.rec
            .write(
                &ctx.run_id,
                "outcome",
                json!({"kind": kind, "text": truncate(&text, TEXT_PREVIEW_CAP)}),
            )
            .await;
        let mut map = self.rec.subject_to_run.lock().await;
        map.remove(ctx.subject.id());
    }
}

pub struct RecordingEmbedder {
    rec: Arc<FlightRecorder>,
    inner: Arc<dyn EmbeddingProvider>,
}

#[async_trait]
impl EmbeddingProvider for RecordingEmbedder {
    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn model_id(&self) -> &ModelId {
        self.inner.model_id()
    }

    async fn embed(
        &self,
        req: EmbeddingRequest,
        ctx: &RunCtx,
        cancel: Option<&CancellationToken>,
    ) -> Result<EmbeddingResponse, EmbeddingError> {
        let texts_preview: Vec<String> = req
            .texts
            .iter()
            .map(|t| truncate(t, TEXT_PREVIEW_CAP))
            .collect();
        self.rec
            .write(
                &ctx.run_id,
                "embedding_request",
                json!({
                    "model": req.model.as_ref().map(ModelId::as_str),
                    "texts": texts_preview,
                }),
            )
            .await;
        let resp = self.inner.embed(req, ctx, cancel).await;
        match &resp {
            Ok(r) => {
                let first_norm = r
                    .vectors
                    .first()
                    .map(|v| v.iter().map(|x| x * x).sum::<f32>().sqrt());
                self.rec
                    .write(
                        &ctx.run_id,
                        "embedding_response",
                        json!({
                            "model": r.model.as_str(),
                            "dim": r.dim,
                            "vector_count": r.vectors.len(),
                            "first_vector_l2_norm": first_norm,
                            "usage": r.usage.map(|u| json!({"prompt_tokens": u.prompt_tokens})),
                        }),
                    )
                    .await;
            }
            Err(err) => {
                self.rec
                    .write(
                        &ctx.run_id,
                        "embedding_error",
                        json!({"error": err.to_string()}),
                    )
                    .await;
            }
        }
        resp
    }
}

pub struct RecordingSink {
    rec: Arc<FlightRecorder>,
    inner: Arc<dyn ChannelSink>,
}

impl RecordingSink {
    async fn record_with_subject(&self, subject: &Subject, kind: &str, payload: Value) {
        if let Some(run_id) = self.rec.run_id_for(subject).await {
            self.rec.write(&run_id, kind, payload).await;
        } else {
            warn!(
                kind = kind,
                subject_id = subject.id(),
                "flight: sink event without active run mapping; dropped",
            );
        }
    }
}

#[async_trait]
impl ChannelSink for RecordingSink {
    async fn send(
        &self,
        ctx: &Subject,
        conv: &Owner,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        self.record_with_subject(
            ctx,
            "sink_send",
            json!({
                "conv": conv.as_str(),
                "body": truncate(&msg.body, TEXT_PREVIEW_CAP),
                "thread_parent": msg.thread_parent.as_ref().map(message_ref_to_json),
                "metadata": msg.metadata,
            }),
        )
        .await;
        let result = self.inner.send(ctx, conv, msg).await;
        if let Err(err) = &result {
            self.record_with_subject(ctx, "sink_send_error", json!({"error": err.to_string()}))
                .await;
        }
        result
    }

    async fn react(
        &self,
        ctx: &Subject,
        conv: &Owner,
        parent: &MessageRef,
        emoji: &str,
    ) -> Result<MessageRef, ChannelError> {
        self.record_with_subject(
            ctx,
            "sink_react",
            json!({
                "conv": conv.as_str(),
                "parent": message_ref_to_json(parent),
                "emoji": emoji,
            }),
        )
        .await;
        self.inner.react(ctx, conv, parent, emoji).await
    }

    async fn edit(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
        new_text: &str,
    ) -> Result<(), ChannelError> {
        self.record_with_subject(
            ctx,
            "sink_edit",
            json!({
                "conv": conv.as_str(),
                "target": message_ref_to_json(target),
                "new_text": truncate(new_text, TEXT_PREVIEW_CAP),
            }),
        )
        .await;
        self.inner.edit(ctx, conv, target, new_text).await
    }

    async fn delete(
        &self,
        ctx: &Subject,
        conv: &Owner,
        target: &MessageRef,
    ) -> Result<(), ChannelError> {
        self.record_with_subject(
            ctx,
            "sink_delete",
            json!({
                "conv": conv.as_str(),
                "target": message_ref_to_json(target),
            }),
        )
        .await;
        self.inner.delete(ctx, conv, target).await
    }

    async fn upload(
        &self,
        ctx: &Subject,
        conv: &Owner,
        filename: &str,
        bytes: Vec<u8>,
        comment: Option<&str>,
        thread_parent: Option<&MessageRef>,
    ) -> Result<MessageRef, ChannelError> {
        self.record_with_subject(
            ctx,
            "sink_upload",
            json!({
                "conv": conv.as_str(),
                "filename": filename,
                "byte_len": bytes.len(),
                "comment": comment,
                "thread_parent": thread_parent.map(message_ref_to_json),
            }),
        )
        .await;
        self.inner
            .upload(ctx, conv, filename, bytes, comment, thread_parent)
            .await
    }

    async fn read(
        &self,
        ctx: &Subject,
        conv: &Owner,
        thread_parent: Option<&MessageRef>,
        limit: usize,
    ) -> Result<Vec<ReadMessage>, ChannelError> {
        self.record_with_subject(
            ctx,
            "sink_read",
            json!({
                "conv": conv.as_str(),
                "thread_parent": thread_parent.map(message_ref_to_json),
                "limit": limit,
            }),
        )
        .await;
        self.inner.read(ctx, conv, thread_parent, limit).await
    }

    async fn notify_user(
        &self,
        ctx: &Subject,
        recipient: &ParticipantId,
        msg: &OutboundMessage,
    ) -> Result<MessageRef, ChannelError> {
        self.record_with_subject(
            ctx,
            "sink_notify_user",
            json!({
                "recipient": recipient.as_str(),
                "body": truncate(&msg.body, TEXT_PREVIEW_CAP),
                "metadata": msg.metadata,
            }),
        )
        .await;
        self.inner.notify_user(ctx, recipient, msg).await
    }
}

fn message_ref_to_json(m: &MessageRef) -> Value {
    json!({
        "channel": m.channel,
        "conv": m.conv.as_str(),
        "id": m.id,
        "thread_root": m.thread_root,
        "broadcast": m.broadcast,
    })
}

fn redact_large_inline_bytes(mut value: Value) -> Value {
    redact_large_inline_bytes_inner(&mut value);
    value
}

fn redact_large_inline_bytes_inner(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let key_lower = key.to_ascii_lowercase();
                if key_lower.contains("base64") {
                    *value = redaction_value(value);
                } else {
                    redact_large_inline_bytes_inner(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_large_inline_bytes_inner(value);
            }
        }
        Value::String(text) if looks_like_large_base64(text) => {
            *value = redaction_value(value);
        }
        _ => {}
    }
}

fn redaction_value(value: &Value) -> Value {
    let len = value.as_str().map_or(0, str::len);
    Value::String(format!("[REDACTED base64 len={len}]"))
}

fn looks_like_large_base64(text: &str) -> bool {
    const MIN_BASE64_LEN: usize = 8192;
    if text.len() < MIN_BASE64_LEN {
        return false;
    }
    text.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'\r' | b'\n'))
}

fn last_user_text(messages: &[Value]) -> Option<String> {
    let last = messages.last()?;
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = last.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let arr = content.as_array()?;
    let mut buf = String::new();
    for block in arr {
        if block.get("type").and_then(Value::as_str) == Some("text")
            && let Some(text) = block.get("text").and_then(Value::as_str)
        {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(text);
        }
    }
    if buf.is_empty() { None } else { Some(buf) }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::from(&s[..end]);
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_preserves_short() {
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn truncate_cuts_long_at_char_boundary() {
        let s = "abcdefghijklmnop";
        let t = truncate(s, 8);
        assert!(t.ends_with("..."));
        assert!(t.starts_with("abcdefgh"));
    }

    #[test]
    fn last_user_text_string_content() {
        let msgs = vec![serde_json::json!({"role": "user", "content": "hi"})];
        assert_eq!(last_user_text(&msgs).as_deref(), Some("hi"));
    }

    #[test]
    fn last_user_text_array_content() {
        let msgs = vec![serde_json::json!({
            "role": "user",
            "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
        })];
        assert_eq!(last_user_text(&msgs).as_deref(), Some("a\nb"));
    }

    #[test]
    fn last_user_text_skips_non_user_trailing() {
        let msgs = vec![
            serde_json::json!({"role": "user", "content": "x"}),
            serde_json::json!({"role": "assistant", "content": "y"}),
        ];
        assert!(last_user_text(&msgs).is_none());
    }

    #[test]
    fn redact_large_inline_bytes_hides_base64_fields() {
        let value = serde_json::json!({
            "args": {
                "content_base64": "aGk=",
                "filename": "a.txt"
            }
        });

        let redacted = redact_large_inline_bytes(value);

        assert_eq!(
            redacted["args"]["content_base64"],
            "[REDACTED base64 len=4]"
        );
        assert_eq!(redacted["args"]["filename"], "a.txt");
    }
}
