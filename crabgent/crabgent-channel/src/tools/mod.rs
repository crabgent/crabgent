//! Built-in channel tools the LLM can call.
//!
//! `ChannelSendTool` posts a message via a `ChannelSink`.
//! `ChannelReactTool` posts an emoji reaction to the user's inbound
//! message. `ChannelListParticipantsTool` enumerates a conversation's
//! participants. `ChannelEditTool`, `ChannelDeleteTool`,
//! `ChannelUploadTool`, and `ChannelReadTool` expose adapter-neutral
//! message operations. All are reference implementations: consumer-side
//! crates can ship their own variants with adapter-specific schemas.

mod delete;
mod edit;
mod notify_user;
mod participants;
mod react;
mod read;
mod send;
mod upload;
mod vision_file;

use crabgent_core::action::Action;
use crabgent_core::error::{KernelError, ToolError};
use crabgent_core::message::Message;
use crabgent_core::owner::Owner;
use crabgent_core::policy::{PolicyDecision, PolicyHook};
use crabgent_core::tool::{Tool, ToolCtx};
use crabgent_core::types::ToolResult;
use serde_json::{Value, json};

use crate::action::channel_name_from_owner;
use crate::envelope::MessageRef;
use crate::error::ChannelError;

pub use delete::ChannelDeleteTool;
pub use edit::ChannelEditTool;
pub use notify_user::NotifyUserTool;
pub use participants::ChannelListParticipantsTool;
pub use react::ChannelReactTool;
pub use read::ChannelReadTool;
pub use send::ChannelSendTool;
pub use upload::ChannelUploadTool;
pub use vision_file::VisionFileTool;

async fn gate_tool(
    policy: &dyn PolicyHook,
    ctx: &ToolCtx,
    tool_name: &'static str,
) -> Result<(), ToolError> {
    match policy.allow(&ctx.subject, &Action::tool(tool_name)).await {
        PolicyDecision::Allow => Ok(()),
        PolicyDecision::Deny(reason) => Err(ToolError::Permission(reason)),
    }
}

fn soft_result(result: Result<Value, ToolError>) -> Result<ToolResult, ToolError> {
    match result {
        Ok(value) => Ok(ToolResult::success(value)),
        Err(ToolError::Permission(reason) | ToolError::InvalidArgs(reason)) => {
            Ok(ToolResult::soft_error(json!(reason)))
        }
        Err(ToolError::Cancelled) => Err(ToolError::Cancelled),
        Err(error) => Ok(ToolResult::soft_error(json!(error.to_string()))),
    }
}

fn message_ref_from_id(
    conv: &Owner,
    id: impl Into<String>,
    thread_root: Option<String>,
    broadcast: bool,
) -> Result<MessageRef, ToolError> {
    let Some(channel) = channel_name_from_owner(conv) else {
        return Err(ToolError::InvalidArgs(
            "conv must be '<channel>:<rest>' format".into(),
        ));
    };
    Ok(MessageRef {
        channel: channel.to_owned(),
        conv: conv.clone(),
        id: id.into(),
        thread_root,
        broadcast,
    })
}

fn render_message_ref(r: &MessageRef) -> Value {
    json!({
        "channel": r.channel,
        "conv": r.conv.as_str(),
        "id": r.id,
        "thread_root": r.thread_root,
        "broadcast": r.broadcast,
    })
}

fn channel_outbound_message(body: String, message_ref: &Value) -> Option<Message> {
    let channel = message_ref.get("channel")?.as_str()?.to_owned();
    let conv = Owner::new(message_ref.get("conv")?.as_str()?);
    let message_id = message_ref.get("id")?.as_str()?.to_owned();
    let thread_root = message_ref
        .get("thread_root")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let broadcast = message_ref
        .get("broadcast")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Some(Message::ChannelOutbound {
        conv,
        body,
        channel,
        message_id,
        thread_root,
        broadcast,
    })
}

#[derive(Clone, Copy)]
enum MessageRefLocation {
    Output,
    Field(&'static str),
}

fn soft_result_with_outbound(
    result: Result<Value, ToolError>,
    body: Option<String>,
    location: MessageRefLocation,
) -> Result<ToolResult, ToolError> {
    let mut result = soft_result(result)?;
    if result.is_error {
        return Ok(result);
    }
    let message_ref = match location {
        MessageRefLocation::Output => Some(&result.output),
        MessageRefLocation::Field(field) => result.output.get(field),
    };
    if let Some((body, message_ref)) = body.zip(message_ref)
        && let Some(message) = channel_outbound_message(body, message_ref)
    {
        result = result.with_run_message(message);
    }
    Ok(result)
}

async fn execute_result_with_outbound<T: Tool + ?Sized>(
    tool: &T,
    args: Value,
    ctx: &ToolCtx,
    body_field: &str,
    location: MessageRefLocation,
) -> Result<ToolResult, ToolError> {
    let body = args
        .get(body_field)
        .and_then(Value::as_str)
        .map(str::to_owned);
    soft_result_with_outbound(tool.execute(args, ctx).await, body, location)
}

#[cfg(test)]
fn assert_single_outbound(
    result: &ToolResult,
    expected_conv: &str,
    expected_body: &str,
    expected_channel: &str,
    expected_id: &str,
) {
    assert_eq!(result.run_messages.len(), 1);
    match &result.run_messages[0] {
        Message::ChannelOutbound {
            conv,
            body,
            channel,
            message_id,
            ..
        } => {
            assert_eq!(conv.as_str(), expected_conv);
            assert_eq!(body, expected_body);
            assert_eq!(channel, expected_channel);
            assert_eq!(message_id, expected_id);
        }
        other => panic!("expected ChannelOutbound, got {other:?}"),
    }
}

fn channel_error_to_tool_error(err: ChannelError) -> ToolError {
    match err {
        // `PolicyDenied` rides the Soft-Deny path: `ToolError::Permission` is
        // what the kernel converts to a `ToolResult { is_error: true }` with
        // the reason carried untruncated into LLM message history. Mapping it
        // to `Execution` here hard-aborts the run instead, dropping the deny
        // reason and breaking the recovery loop the LLM is supposed to enter.
        // Return a soft denial instead of exposing authorization internals.
        ChannelError::PolicyDenied { action: _, reason } => ToolError::Permission(reason),
        ChannelError::InvalidOwnerFormat(owner) => ToolError::InvalidArgs(owner),
        ChannelError::InvalidEnvelope(_) => ToolError::InvalidArgs("invalid envelope".into()),
        ChannelError::NotRegistered(name) | ChannelError::ConversationNotFound(name) => {
            ToolError::NotFound(name)
        }
        // Cancellation / shutdown must not soft-wrap into an LLM-recoverable
        // `Execution` payload via `soft_result`: the kernel run is on the way
        // out, so let the cancel signal propagate. Sweep-3 (`7f95edf`) widened
        // `soft_result` coverage to 4 more tools and accidentally pulled both
        // variants into the recoverable path through the catch-all below;
        // the cancellation fix restores the hard-abort by mapping to
        // `ToolError::Cancelled`. The nested `Kernel(_)` arm unwraps the same
        // signal when an inner kernel run rather than the channel itself
        // surfaces the cancel.
        ChannelError::Cancelled
        | ChannelError::ShuttingDown
        | ChannelError::Kernel(KernelError::Cancelled | KernelError::ShuttingDown) => {
            ToolError::Cancelled
        }
        ChannelError::Kernel(_) => ToolError::Execution("kernel error".into()),
        ChannelError::Serde(_) => ToolError::Execution("serialization error".into()),
        other => ToolError::Execution(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_denied_maps_to_permission_carrying_implementor_reason() {
        // The implementor-supplied reason from `PolicyDecision::Deny(reason)`
        // must reach the LLM verbatim via `ToolError::Permission`, not the
        // generic "policy denied: <action>" fallback. The action name is
        // intentionally dropped here because the implementor is expected
        // to encode whatever context they want into the reason string.
        let err = channel_error_to_tool_error(ChannelError::PolicyDenied {
            action: "channel.send".into(),
            reason: "scope not in allowed set for direct channel".into(),
        });
        match err {
            ToolError::Permission(reason) => {
                assert_eq!(reason, "scope not in allowed set for direct channel");
            }
            other => panic!("expected Permission, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_maps_to_execution() {
        let err = channel_error_to_tool_error(ChannelError::Unsupported("react"));
        assert!(matches!(err, ToolError::Execution(_)));
    }

    #[test]
    fn invalid_owner_maps_to_invalid_args() {
        let err = channel_error_to_tool_error(ChannelError::InvalidOwnerFormat("nope".into()));
        assert!(matches!(err, ToolError::InvalidArgs(_)));
    }

    #[test]
    fn not_registered_maps_to_not_found() {
        let err = channel_error_to_tool_error(ChannelError::NotRegistered("matrix".into()));
        assert!(matches!(err, ToolError::NotFound(_)));
    }

    #[test]
    fn cancelled_maps_to_cancelled_not_execution() {
        let err = channel_error_to_tool_error(ChannelError::Cancelled);
        assert!(
            matches!(err, ToolError::Cancelled),
            "ChannelError::Cancelled must short-circuit, not soft-wrap into Execution",
        );
    }

    #[test]
    fn shutting_down_maps_to_cancelled_not_execution() {
        let err = channel_error_to_tool_error(ChannelError::ShuttingDown);
        assert!(
            matches!(err, ToolError::Cancelled),
            "ChannelError::ShuttingDown must short-circuit, not soft-wrap into Execution",
        );
    }

    #[test]
    fn nested_kernel_cancelled_unwraps_to_cancelled() {
        let err = channel_error_to_tool_error(ChannelError::Kernel(KernelError::Cancelled));
        assert!(
            matches!(err, ToolError::Cancelled),
            "ChannelError::Kernel(KernelError::Cancelled) must unwrap to Cancelled",
        );
        let err = channel_error_to_tool_error(ChannelError::Kernel(KernelError::ShuttingDown));
        assert!(
            matches!(err, ToolError::Cancelled),
            "ChannelError::Kernel(KernelError::ShuttingDown) must unwrap to Cancelled",
        );
    }

    #[test]
    fn nested_kernel_non_cancel_maps_to_opaque_execution() {
        let err = channel_error_to_tool_error(ChannelError::Kernel(KernelError::Internal(
            "secret token should stay operator-only".into(),
        )));
        match err {
            ToolError::Execution(message) => {
                assert_eq!(message, "kernel error");
                assert!(!message.contains("secret"));
            }
            other => panic!("expected opaque Execution, got {other:?}"),
        }
    }

    #[test]
    fn serde_maps_to_opaque_execution() {
        let serde_err =
            serde_json::from_str::<Value>("{").expect_err("invalid JSON should fail to parse");
        let err = channel_error_to_tool_error(ChannelError::Serde(serde_err));
        match err {
            ToolError::Execution(message) => assert_eq!(message, "serialization error"),
            other => panic!("expected opaque Execution, got {other:?}"),
        }
    }

    #[test]
    fn soft_result_propagates_cancelled_as_hard_error() {
        let err = soft_result(Err(ToolError::Cancelled)).expect_err("cancel should stay hard");
        assert!(matches!(err, ToolError::Cancelled));
    }
}
