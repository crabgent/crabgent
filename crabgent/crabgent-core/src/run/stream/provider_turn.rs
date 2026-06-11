//! Provider-stream pumping for one LLM turn.

use futures::StreamExt;
use tokio::sync::mpsc;

use crate::error::KernelError;
use crate::hook::Event;
use crate::message::Message;
use crate::model::{ModelId, ResolvedEffort, ResolvedModelWithSource};
use crate::provider::ProviderEvent;
use crate::types::{LlmRequest, LlmResponse, StopReason, ToolCall, Usage};

use super::super::fallback::{FallbackEnv, OpenedStream, retry_pump_with_fallbacks};
use super::super::model_resolution::ResolvedModel;
use super::StreamCfg;
use super::events::emit_event;

pub(super) struct ProviderTurn {
    pub request: LlmRequest,
    pub response: LlmResponse,
    pub current_model: ResolvedModelWithSource,
    pub current_effort: ResolvedEffort,
    /// `Message::ProviderBlock` entries produced by `ProviderEvent::ServerToolResult`
    /// during this turn. The caller appends them to the message log so the next
    /// turn can replay them for providers that require it.
    pub server_tool_blocks: Vec<Message>,
}

struct StreamAccum<'a> {
    text: &'a mut String,
    tool_calls: &'a mut Vec<ToolCall>,
    stop_reason: &'a mut StopReason,
    usage: &'a mut Usage,
    model: &'a mut ModelId,
    server_tool_blocks: &'a mut Vec<Message>,
}

impl StreamAccum<'_> {
    fn reset_for_retry(&mut self, model: ModelId) {
        self.text.clear();
        self.tool_calls.clear();
        self.server_tool_blocks.clear();
        *self.stop_reason = StopReason::EndTurn;
        *self.usage = Usage::default();
        *self.model = model;
    }
}

pub(super) async fn consume_provider_stream(
    cfg: &StreamCfg,
    base_req: &LlmRequest,
    attempts: &[ResolvedModel],
    opened: OpenedStream,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
) -> Result<ProviderTurn, KernelError> {
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut stop_reason = StopReason::EndTurn;
    let mut usage = Usage::default();
    let mut model = opened.request.model.clone();
    let mut server_tool_blocks: Vec<Message> = Vec::new();

    let opened = tokio::select! {
        biased;
        () = cfg.cancel.cancelled() => return Err(KernelError::Cancelled),
        () = tx.closed() => return Err(KernelError::Cancelled),
        result = pump_provider_events(
            cfg,
            base_req,
            attempts,
            opened,
            tx,
            StreamAccum {
                text: &mut text,
                tool_calls: &mut tool_calls,
                stop_reason: &mut stop_reason,
                usage: &mut usage,
                model: &mut model,
                server_tool_blocks: &mut server_tool_blocks,
            },
        ) => result?,
    };

    Ok(ProviderTurn {
        request: opened.request,
        response: LlmResponse {
            text,
            tool_calls,
            stop_reason,
            usage,
            model,
        },
        current_model: opened.current_model,
        current_effort: opened.current_effort,
        server_tool_blocks,
    })
}

async fn pump_provider_events(
    cfg: &StreamCfg,
    base_req: &LlmRequest,
    attempts: &[ResolvedModel],
    mut current: OpenedStream,
    tx: &mpsc::Sender<Result<Event, KernelError>>,
    mut accum: StreamAccum<'_>,
) -> Result<OpenedStream, KernelError> {
    loop {
        let ev = tokio::select! {
            biased;
            () = cfg.cancel.cancelled() => return Err(KernelError::Cancelled),
            () = tx.closed() => return Err(KernelError::Cancelled),
            ev = current.stream.next() => ev,
        };
        let Some(ev) = ev else {
            return Ok(current);
        };
        match ev {
            Err(err) => {
                let env = FallbackEnv {
                    providers: cfg.providers.as_ref(),
                    registry: cfg.models.as_ref(),
                    base: base_req,
                    attempts,
                    model_source: current.current_model.source,
                    effort_source: current.resolved_effort_source,
                    ctx: &cfg.run_ctx,
                    cancel: Some(&cfg.cancel),
                    hooks: &cfg.hooks,
                    stream_tx: Some(tx),
                };
                current = retry_pump_with_fallbacks(&env, current.attempt_idx, err).await?;
                accum.reset_for_retry(current.request.model.clone());
            }
            Ok(event) => match event {
                ProviderEvent::TextDelta(s) => {
                    accum.text.push_str(&s);
                    emit_event(&cfg.hooks, &cfg.run_ctx, Event::Token(s), tx).await?;
                }
                ProviderEvent::ReasoningDelta(s) => {
                    emit_event(&cfg.hooks, &cfg.run_ctx, Event::Reasoning(s), tx).await?;
                }
                ProviderEvent::ToolUse(c) => accum.tool_calls.push(c),
                ProviderEvent::Usage(u) => *accum.usage = u,
                ProviderEvent::Stop(s) => *accum.stop_reason = s,
                ProviderEvent::ServerToolResult {
                    provider,
                    name,
                    content,
                    citations,
                } => {
                    // Record the raw provider block so the next turn can replay
                    // it for providers that require server-tool-result history.
                    accum.server_tool_blocks.push(Message::ProviderBlock {
                        provider: provider.clone(),
                        block: content.clone(),
                    });
                    emit_event(
                        &cfg.hooks,
                        &cfg.run_ctx,
                        crate::hook::Event::ServerToolResult {
                            provider,
                            name,
                            citations,
                            raw: content,
                        },
                        tx,
                    )
                    .await?;
                }
            },
        }
    }
}
