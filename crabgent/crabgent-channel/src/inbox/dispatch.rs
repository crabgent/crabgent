//! Dispatch entry: policy + claim + spawn. See run.rs for kernel-run helpers.

use std::sync::Arc;

use crabgent_core::owner::Owner;
use crabgent_core::policy::PolicyDecision;

use crate::action::channel_receive_action;
use crate::error::ChannelError;
use crate::inbox_lifecycle::ClaimResult;

use super::KernelChannelInbox;
use super::ingress::IngressPlan;
use super::run::{LiveRunParams, run_kernel_with_release};

pub(super) async fn dispatch_request(
    state: &KernelChannelInbox,
    plan: IngressPlan,
) -> Result<(), ChannelError> {
    let IngressPlan {
        mut req,
        conv_key,
        inject_value,
    } = plan;
    if state.lifecycle.is_shutdown() {
        return Err(ChannelError::ShuttingDown);
    }

    let conv_owner = Owner::new(conv_key.1.as_str());
    let action = channel_receive_action(&conv_key.0, &conv_owner);
    match state.policy.allow(&req.subject, &action).await {
        PolicyDecision::Allow => {}
        PolicyDecision::Deny(reason) => {
            return Err(ChannelError::policy_denied(action.name(), reason));
        }
    }

    let run_id = req.run_id.clone();
    let (cancel, cancel_reason) = match state
        .lifecycle
        .try_claim_conv(conv_key.clone(), run_id)
        .await
    {
        ClaimResult::Existing(existing_run_id) => {
            state
                .inject_registry
                .submit(&existing_run_id, inject_value)
                .await;
            return Ok(());
        }
        ClaimResult::Spawned {
            cancel,
            cancel_reason,
        } => (cancel, cancel_reason),
    };
    req.cancel_reason = Some(cancel_reason);

    let permit = state.lifecycle.acquire_permit().await?;
    let kernel = Arc::clone(&state.kernel);
    let lifecycle = Arc::clone(&state.lifecycle);
    let live_params = LiveRunParams {
        live_turn: state.live_turn.clone(),
        subject: req.subject.clone(),
        conv: conv_owner,
        channel: conv_key.0.clone(),
    };
    state
        .lifecycle
        .spawn_run(
            req.run_id.clone(),
            run_kernel_with_release(
                kernel,
                req,
                cancel,
                permit,
                lifecycle,
                conv_key,
                live_params,
            ),
        )
        .await?;
    Ok(())
}
