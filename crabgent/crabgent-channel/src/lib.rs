//! Channel abstraction for crabgent.
//!
//! Provides a `Channel` trait plus supporting types so external
//! adapters (Slack, Telegram, Signal, web, ...) can plug into the
//! kernel without leaking adapter-specific details. Two variants:
//! `ChannelKind::Group` for multi-participant conversations and
//! `ChannelKind::Direct` for 1:1 exchanges (human-agent, agent-agent,
//! agent-human).
//!
//! Policy decisions go through the existing `PolicyHook`. Channel
//! actions carry typed channel and conversation targets so policies can
//! match the requested destination, while incoming channel context also
//! lives in `Subject::attrs`. See `crate::action` for helpers.
//!
//! Threading is opaque: a `MessageRef` exposes only `TopLevel` vs
//! `ThreadReply { root, broadcast }`. Adapter-specific shapes (Slack
//! `thread_ts`, Telegram `reply_to_message_id`) stay inside the
//! per-channel crate.
//!
//! `recording_inbox` is a pre-policy decorator; recorder impls own
//! their policy gating and trust-boundary checks.

/// Keep this alias in sync with `tracing_test::traced_test` and `#[instrument]`
/// macro expectations, so tests keep using the canonical `tracing` path while
/// crate internals stay coupled to `crabgent_log`.
extern crate crabgent_log as tracing;

pub mod action;
pub mod audio_store;
pub mod audio_validation;
pub mod channel;
pub mod envelope;
pub mod error;
pub mod image_store;
pub mod image_validation;
pub mod inbound;
pub mod inbox;
mod inbox_lifecycle;
pub mod media_assembly;
pub mod media_download;
pub mod pairing;
pub mod participant;
pub mod recording_inbox;
pub mod sink;
pub mod speaker_id;
pub mod startup_cutoff_inbox;
pub mod stop_pattern;
pub mod stt_inbox;
pub mod subject;
pub mod tools;
pub mod webhook;

#[cfg(test)]
mod test_support;

pub use action::{
    CHANNEL_LIST_PARTICIPANTS, CHANNEL_RECEIVE, CHANNEL_SEND, ChannelPolicyExt, ChannelRuleBuilder,
    IntoChannelFilter, channel_list_participants_action, channel_receive_action,
    channel_send_action,
};
pub use audio_store::sweeper::AudioStoreSweeper;
pub use audio_store::{AudioStore, AudioStoreError};
pub use audio_validation::{ALLOWED_AUDIO_MIMES, AudioRejection, AudioValidator, MAX_AUDIO_BYTES};
pub use channel::{Channel, ChannelKind, ConvLabel, ReadMessage};
pub use envelope::{InboundEvent, InboundReaction, MessageKind, MessageRef, OutboundMessage};
pub use error::ChannelError;
pub use image_store::{ImageRef, ImageStore, ImageStoreError};
pub use image_validation::{
    ImageRejection, ImageValidator, MAX_IMAGE_BYTES, image_rejection_fallback_text,
};
pub use inbound::{InboundBody, InboundEventBuilder, InboundParticipant};
pub use inbox::{
    ChannelInbox, FinalDeliveryPolicy, INBOUND_BODY_MAX_BYTES, KernelChannelInbox,
    LiveProgressMode, LiveTurnConfig, check_inbound_size, sanitize_for_prompt,
    subject_from_inbound_event,
};
pub use media_assembly::{
    AudioAssemblyError, IMAGE_PROCESSING_FALLBACK_BODY, assemble_audio_attachment,
    assemble_image_attachment, image_download_size_fallback, image_processing_fallback,
};
pub use media_download::{CappedMediaBody, MediaBodyTooLarge, MediaDownloadError};
pub use pairing::{FilePairingStore, MemoryPairingStore, PairingInbox, PairingStore};
pub use participant::{DirectRole, Participant, ParticipantId, ParticipantRole};
pub use recording_inbox::{InboundRecorder, RecordDecision, RecordingInbox};
pub use sink::{ChannelRouter, ChannelSink};
pub use speaker_id::{SpeakerIdentificationError, SpeakerIdentificationRequest, SpeakerIdentifier};
pub use startup_cutoff_inbox::StartupCutoffInbox;
pub use stop_pattern::StopPatternMatcher;
pub use stt_inbox::{AUDIO_TRANSCRIPT_PREFIX, SttInbox};
pub use subject::{
    ChannelAttr, ChannelSubjectExt, InboundReactionAttr, attr_keys, channel_subject_id,
    parse_channel_subject_id,
};
pub use tools::{
    ChannelDeleteTool, ChannelEditTool, ChannelListParticipantsTool, ChannelReactTool,
    ChannelReadTool, ChannelSendTool, ChannelUploadTool, NotifyUserTool, VisionFileTool,
};
pub use webhook::{
    NoopSignatureVerify, SignatureVerify, WebhookHandler, WebhookRequest, WebhookResponse,
};
