use crabgent_core::ModelTarget;
use crabgent_core::message::{ContentBlock, Message};
use crabgent_core::run::RunRequest;
use crabgent_core::run_id::RunId;
use crabgent_core::subject::Subject;
use serde_json::Value;

use crate::envelope::InboundEvent;
use crate::error::ChannelError;
use crate::inbox_lifecycle::ConvKey;

use super::{KernelChannelInbox, PERSONA_BOUNDARY_PREFIX, dispatch, hint, run};

pub(super) struct IngressPlan {
    pub req: RunRequest,
    pub conv_key: ConvKey,
    pub inject_value: Value,
}

impl KernelChannelInbox {
    pub(super) fn plan_event_with_subject(
        &self,
        event: &InboundEvent,
        subject: Subject,
    ) -> Result<IngressPlan, ChannelError> {
        let req = self.build_request_with_subject(event, subject);
        Self::plan_from_request(event, req)
    }

    /// Build the ingress plan for `event`, stamping the async-resolved
    /// `conv_display` `label` onto the base subject before the `<inbound>`
    /// tag is rendered. The sender display is stamped by the resolver, so
    /// both the live request and the injection value carry the readable
    /// context. The reaction path stamps the conv labels directly in
    /// `receive_reaction` (it also needs the reaction attrs on the subject).
    pub(super) fn plan_event_with_display(
        &self,
        event: &InboundEvent,
        label: &super::ConvLabel,
    ) -> Result<IngressPlan, ChannelError> {
        let subject = (self.subject_resolver)(event)?;
        let subject = super::subject_resolver::stamp_conv_display(subject, label);
        self.plan_event_with_subject(event, subject)
    }

    fn plan_from_request(
        event: &InboundEvent,
        req: RunRequest,
    ) -> Result<IngressPlan, ChannelError> {
        let conv_key = run::conv_key_for(event);
        // The mid-turn inject value re-renders the `<inbound>` tag a second
        // time, but from the SAME display-stamped `req.subject` that
        // build_request_with_subject used for the live request. Both paths
        // therefore always carry identical channel/name/workspace/sender
        // context: a fresh inbound run and an injected follow-up wrap the user
        // text the same way. This double-render is intentional, not a bug.
        let inject_value = run::event_to_inject_value(event, &req.subject)?;
        Ok(IngressPlan {
            req,
            conv_key,
            inject_value,
        })
    }

    /// Resolve the base subject and build the request in one step.
    ///
    /// Test-only: it skips the async `conv_display` resolution that
    /// `receive`/`receive_reaction` perform, so the production paths go
    /// through `plan_event_with_display` / `plan_event_with_subject`
    /// instead. Tests that want to assert the rendered `<inbound>` tag or
    /// the system prompt use this synchronous shortcut.
    #[cfg(test)]
    pub(super) fn build_request(&self, event: &InboundEvent) -> Result<RunRequest, ChannelError> {
        let subject = (self.subject_resolver)(event)?;
        Ok(self.build_request_with_subject(event, subject))
    }

    pub(super) fn build_request_with_subject(
        &self,
        event: &InboundEvent,
        subject: Subject,
    ) -> RunRequest {
        let system_prompt = self.compose_system_prompt(event, &subject);
        let mut content: Vec<ContentBlock> = vec![ContentBlock::Text {
            text: run::inbound_text_body(event.body.as_str(), &subject),
        }];
        content.extend(
            event
                .attachments
                .iter()
                .map(|block| run::inbound_content_block(block, &subject)),
        );
        RunRequest {
            pause: None,
            run_id: RunId::new(),
            subject,
            model: ModelTarget::id(self.model.clone()),
            explicit_model: None,
            session_model_override: None,
            fallbacks: self.fallbacks.clone(),
            system_prompt,
            messages: vec![Message::user_at(content, event.timestamp)],
            max_turns: self.max_turns,
            temperature: None,
            max_tokens: None,
            cancel_reason: None,
            reasoning_effort: None,
            web_search: ::crabgent_core::types::WebSearchConfig::default(),
        }
    }

    /// Compose the system prompt: the [`PERSONA_BOUNDARY_PREFIX`] at the
    /// head, then base prompt, optional conversation hint, and optional
    /// formatting hint, joined by `"\n\n"`. The prefix is still emitted
    /// when no optional prompt parts are configured.
    ///
    /// `subject` carries the `conv_display` labels the production path
    /// stamped, so the hint names the same readable channel name the
    /// `<inbound>` tag renders.
    pub(super) fn compose_system_prompt(
        &self,
        event: &InboundEvent,
        subject: &Subject,
    ) -> Option<String> {
        let base = self.system_prompt.clone().filter(|s| !s.is_empty());
        let kind = event.kind.or(self.inferred_kind);
        let channel_display = subject.attr(crate::subject::attr_keys::CHANNEL_DISPLAY);
        let conv = self.conversation_hint_enabled.then(|| {
            if self.live_turn.is_some() {
                hint::build_live_conversation_hint(event, kind, channel_display)
            } else {
                hint::build_conversation_hint(event, kind, channel_display)
            }
        });
        let fmt = self.formatting_hint.clone();
        let parts: Vec<String> = [base, conv, fmt]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
        if parts.is_empty() {
            Some(PERSONA_BOUNDARY_PREFIX.to_owned())
        } else {
            Some(format!("{PERSONA_BOUNDARY_PREFIX}{}", parts.join("\n\n")))
        }
    }

    pub(super) async fn dispatch_plan(&self, plan: IngressPlan) -> Result<(), ChannelError> {
        dispatch::dispatch_request(self, plan).await
    }
}
