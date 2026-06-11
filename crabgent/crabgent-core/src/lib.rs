//! # crabgent-core
//!
//! Lean agentic LLM gateway kernel. This crate owns the provider, tool,
//! hook, policy, model registry, memory action, subject, message, and
//! run-loop surfaces used by contrib crates and host applications.
//!
//! The kernel is library-only: hosts wire it into CLIs, HTTP servers,
//! channel adapters, or schedulers. See `PROJECT.md` and `SPIRIT.md` in
//! the repository root for architecture and design rules.

pub mod action;
pub mod activity;
pub mod embedding;
pub mod error;
pub mod forced_alignment;
pub mod hook;
pub mod hook_chain;
pub mod image_generation;
pub mod kernel;
pub mod memory;
pub mod message;
pub mod model;
mod newtype;
pub mod owner;
pub mod policy;
pub mod provider;
pub mod provider_projection;
mod provider_set;
pub mod run;
pub mod run_id;
pub mod sanitize;
pub mod stt;
pub mod subject;
pub mod text;
pub mod tokens;
pub mod tool;
pub mod tts;
pub mod types;
pub mod voice;

pub use action::{Action, ActionTarget};
pub use activity::{
    ACTIVITY_TEXT_PREVIEW_BYTES, ActivityEventSummary, ActivityTextSummary,
    AttemptFailedActivitySummary, JsonShapeSummary, JsonValueKind, NotificationActivitySummary,
    ServerToolResultActivitySummary, ToolCallActivitySummary, ToolCallResultActivitySummary,
};
pub use embedding::{
    EmbeddingError, EmbeddingProvider, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage,
};
pub use error::{KernelError, ProviderError, ToolError};
pub use forced_alignment::{
    ForcedAlignedCharacter, ForcedAlignedWord, ForcedAlignmentError, ForcedAlignmentProvider,
    ForcedAlignmentProviderCapabilities, ForcedAlignmentRequest, ForcedAlignmentResponse,
};
pub use hook::{AttemptErrorClass, CancelReason, Decision, Event, Hook, Outcome, RunCtx};
pub use hook_chain::HookChain;
pub use image_generation::{
    GENERATED_IMAGE_MAX_BYTES, GeneratedImage, ImageGenerationAspectRatio,
    ImageGenerationBackground, ImageGenerationError, ImageGenerationFormat, ImageGenerationModelId,
    ImageGenerationModelInfo, ImageGenerationProvider, ImageGenerationProviderCapabilities,
    ImageGenerationQuality, ImageGenerationRequest, ImageGenerationResponse, ImageGenerationSize,
    ImageGenerationUsage,
};
pub use kernel::{BuilderState, Defaults, Kernel, KernelBuilder, Set, Unset};
pub use memory::{
    DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT, MemoryId, MemoryScope, OwnerMatch, ParseMemoryIdError,
    SearchQuery,
};
pub use message::{
    AUDIO_PAYLOAD_ALLOWED_MIMES, AUDIO_PAYLOAD_MAX_BYTES, AudioPayload, ContentBlock,
    FILE_PAYLOAD_MAX_BYTES, FilePayload, IMAGE_PAYLOAD_MAX_BYTES, ImagePayload, Message,
    PayloadError, RawMessages,
};
pub use model::{
    AmbiguousModelError, DuplicateModelError, EffortSource, GlobalModelOverrideStore,
    GlobalReasoningEffortOverrideStore, ModelCapabilities, ModelCapability, ModelId, ModelInfo,
    ModelOverrideStoreError, ModelRegistry, ModelTarget, NoopGlobalModelOverrideStore,
    NoopGlobalReasoningEffortOverrideStore, Pricing, ReasoningEffort,
    ReasoningEffortOverrideStoreError, ResolveModelError, ResolvedEffort, ResolvedModelWithSource,
    ResolvedSource, UnknownModelError, UnknownModelTargetError,
};
pub use owner::{Owner, ThreadId};
pub use policy::{
    ActionMatcher, AllowAllPolicy, DenyAllPolicy, PolicyDecision, PolicyHook, Rule, StrictPolicy,
    StrictPolicyBuilder, TargetPredicate,
};
pub use provider::{EventStream, Provider, ProviderCapabilities, ProviderEvent};
pub use provider_projection::{
    ProjectedContent, ProjectedToolCall, ProjectedTurn, project_conversation,
};
pub use provider_set::BuildError;
pub use run::RunRequest;
pub use run_id::{ParseRunIdError, RunId};
pub use stt::{
    DuplicateSttModelError, SttError, SttEvent, SttEventStream, SttModelId, SttModelInfo,
    SttModelRegistry, SttProvider, SttProviderCapabilities, SttRequest, SttResponse, SttSegment,
    SttWord, UnknownSttModelError,
};
pub use subject::{InvalidSubjectError, Subject};
pub use tool::{BashTool, ReadFileTool, Tool, ToolCtx, UpdateFileTool, WriteFileTool};
pub use tts::{
    TtsAudioFormat, TtsError, TtsModelId, TtsModelInfo, TtsProvider, TtsProviderCapabilities,
    TtsRequest, TtsResponse, VoiceId,
};
pub use types::{
    Citation, LlmRequest, LlmResponse, Notification, NotificationLevel, StopReason, ToolAccess,
    ToolCall, ToolChoice, ToolDef, ToolResult, Usage, WebSearchConfig,
};
pub use voice::{AudioEvent, AudioRef, EnergyBand, SpeakerIdentity, VoiceSignals};
