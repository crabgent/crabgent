//! `ChannelInbox` trait impl for `KernelChannelInbox`. Lives in its own
//! file so `inbox/mod.rs` stays under the 500-LOC cap.

use std::time::Duration;

use async_trait::async_trait;
use crabgent_log::instrument;

use super::run::synth_event_from_reaction;
use super::subject_resolver::stamp_conv_display;
use super::{ChannelInbox, KernelChannelInbox};
use crate::envelope::{InboundEvent, InboundReaction, OutboundMessage};
use crate::error::ChannelError;
use crate::subject::ChannelSubjectExt;

#[async_trait]
impl ChannelInbox for KernelChannelInbox {
    #[instrument(level = "debug", skip(self, event), fields(channel = %event.channel))]
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        // Resolve the readable conversation labels once, before the subject
        // is built, so a single await keeps the dispatch hot-path from
        // serializing. A missing channel or a failed lookup yields an empty
        // label and the tag simply omits the optional attrs.
        let label = self.resolve_conv_display(&event.conv).await;
        // Build the request first so the policy-relevant subject is resolved
        // before any active-runs claim. A policy-denied event never mutates
        // tracked state.
        let plan = self.plan_event_with_display(&event, &label)?;
        let conv_key = plan.conv_key.clone();
        if !self.stop_matcher.is_empty() && self.stop_matcher.matches(&event.body) {
            let cancelled = self.lifecycle.cancel_conv(&conv_key).await;
            if cancelled && let Some(sink) = self.cancel_ack_sink.as_ref() {
                let ack = OutboundMessage::new("Cancelled.").with_metadata("channel", &conv_key.0);
                if let Err(err) = sink.send(&plan.req.subject, &event.conv, &ack).await {
                    crabgent_log::warn!(target: "crabgent_channel.stop", ?err, "cancel-ack send failed");
                }
            }
            return Ok(());
        }
        self.dispatch_plan(plan).await
    }

    #[instrument(level = "debug", skip(self, reaction), fields(channel = %reaction.channel, added = reaction.added))]
    async fn receive_reaction(&self, reaction: InboundReaction) -> Result<(), ChannelError> {
        let event = synth_event_from_reaction(&reaction);
        // Same single-await label resolution as `receive`: the reaction
        // path shares the subject-stamp pipeline, so it inherits the
        // readable channel context for free.
        let label = self.resolve_conv_display(&event.conv).await;
        let subject = stamp_conv_display((self.subject_resolver)(&event)?, &label)
            .with_inbound_reaction(&reaction);
        let plan = self.plan_event_with_subject(&event, subject)?;
        // Stop-pattern intentionally not applied: reactions carry emoji
        // codes, not user text.
        self.dispatch_plan(plan).await
    }

    /// Forward kernel-channel shutdown to the inbox lifecycle.
    ///
    /// The caller-supplied `grace` is honored as the drain deadline,
    /// with one exception: `Duration::ZERO` is the canonical sentinel
    /// for "use the lifecycle's configured `shutdown_grace`", set via
    /// [`KernelChannelInbox::with_shutdown_grace`]. Any positive
    /// duration overrides the lifecycle default for this call only.
    async fn shutdown(&self, grace: Duration) {
        self.lifecycle.shutdown_with_grace(grace).await;
    }
}
