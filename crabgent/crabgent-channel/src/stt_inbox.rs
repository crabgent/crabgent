//! Speech-to-text inbox decorator.

use std::sync::Arc;

use async_trait::async_trait;
use crabgent_core::{
    Action, AudioPayload, ContentBlock, MemoryScope, Owner, PolicyDecision, PolicyHook,
    SpeakerIdentity, SttError, SttModelId, SttProvider, SttRequest, SttResponse, Subject,
};

use crabgent_prosody::ProsodyConfig;

use crate::audio_store::AudioStore;
use crate::envelope::InboundEvent;
use crate::error::ChannelError;
use crate::inbox::ChannelInbox;
use crate::speaker_id::{SpeakerIdentificationRequest, SpeakerIdentifier};

/// Prefix prepended to generated transcript text blocks.
pub const AUDIO_TRANSCRIPT_PREFIX: &str = TRANSCRIPT_PREFIX;
const TRANSCRIPT_PREFIX: &str = "[Audio-Transkript]: ";

/// Decorates a channel inbox with speech-to-text handling for audio blocks.
pub struct SttInbox<I: ChannelInbox> {
    stt: Arc<dyn SttProvider>,
    audio_store: Option<Arc<dyn AudioStore>>,
    speaker_identifier: Option<Arc<dyn SpeakerIdentifier>>,
    policy: Option<Arc<dyn PolicyHook>>,
    prosody: ProsodyConfig,
    next: I,
}

impl<I: ChannelInbox> SttInbox<I> {
    pub fn new(stt: Arc<dyn SttProvider>, next: I) -> Self {
        Self {
            stt,
            audio_store: None,
            speaker_identifier: None,
            policy: None,
            prosody: ProsodyConfig::default(),
            next,
        }
    }

    /// Retain inbound audio bytes in `store` and emit a
    /// [`ContentBlock::Transcript`] carrying the handle, instead of
    /// collapsing audio into flat text. Without a store the decorator
    /// keeps the legacy behaviour: no bytes retained, transcripts folded
    /// into a single text block (fail-closed retention).
    #[must_use]
    pub fn with_audio_store(mut self, store: Arc<dyn AudioStore>) -> Self {
        self.audio_store = Some(store);
        self
    }

    /// Enrich retained transcripts with deployment-local speaker identity
    /// guesses.
    ///
    /// Identification is fail-open: recognizer errors are logged and the
    /// transcript still reaches the inner inbox with the base STT/prosody
    /// signals.
    #[must_use]
    pub fn with_speaker_identifier(mut self, identifier: Arc<dyn SpeakerIdentifier>) -> Self {
        self.speaker_identifier = Some(identifier);
        self
    }

    /// Gate raw-audio retention behind a [`PolicyHook`].
    ///
    /// Persisting a subject's voice bytes to disk is privacy-sensitive and
    /// fail-closed: retention happens only when an audio store is configured
    /// AND this policy returns [`PolicyDecision::Allow`] for
    /// [`Action::AudioRetain`] scoped to the speaker. Without a policy the
    /// decorator never retains, even with a store wired, and folds transcripts
    /// into flat text (the no-retention path).
    #[must_use]
    pub fn with_policy(mut self, policy: Arc<dyn PolicyHook>) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Override the default [`ProsodyConfig`] used to derive
    /// [`VoiceSignals`] from each transcription response.
    ///
    /// By default word-level timing is enabled with a hesitation threshold of
    /// 600 ms (matching [`ProsodyConfig::default`]).
    #[must_use]
    pub const fn with_prosody_config(mut self, cfg: ProsodyConfig) -> Self {
        self.prosody = cfg;
        self
    }
}

#[async_trait]
impl<I: ChannelInbox> ChannelInbox for SttInbox<I> {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        let event = self.transcribe_audio(event).await?;
        self.next.receive(event).await
    }

    crate::forward_channel_inbox_methods!(next);
}

impl<I: ChannelInbox> SttInbox<I> {
    async fn transcribe_audio(
        &self,
        mut event: InboundEvent,
    ) -> Result<InboundEvent, ChannelError> {
        if !event
            .attachments
            .iter()
            .any(|block| matches!(block, ContentBlock::Audio(_)))
        {
            return Ok(event);
        }

        let model = self.default_model()?;
        let retain = self.retention_allowed(&event).await;
        let subject = Subject::new(event.from.id.as_str());
        let original = std::mem::take(&mut event.attachments);
        event.attachments = match (&self.audio_store, retain) {
            (Some(store), true) => {
                self.retain_and_transcribe(original, &model, store, &subject)
                    .await?
            }
            _ => self.collapse_transcribe(original, &model).await?,
        };
        Ok(event)
    }

    /// Decide whether this event's audio may be persisted to disk.
    ///
    /// Fail-closed: retention requires both a configured audio store and an
    /// explicit `PolicyHook` grant for [`Action::AudioRetain`] scoped to the
    /// speaker. A missing policy or a deny falls through to the no-retention
    /// (flat-text) path. The denial is logged for the per-subject audit trail;
    /// the policy reason is owned by the implementor and not surfaced here.
    async fn retention_allowed(&self, event: &InboundEvent) -> bool {
        if self.audio_store.is_none() {
            return false;
        }
        let Some(policy) = self.policy.as_ref() else {
            warn_retention_skipped(
                event.from.id.as_str(),
                "no PolicyHook configured (fail-closed)",
            );
            return false;
        };
        let subject = Subject::new(event.from.id.as_str());
        let scope = MemoryScope::for_owner(Owner::new(subject.id()));
        match policy.allow(&subject, &Action::AudioRetain { scope }).await {
            PolicyDecision::Allow => true,
            PolicyDecision::Deny(_) => {
                warn_retention_skipped(event.from.id.as_str(), "denied by policy");
                false
            }
        }
    }

    /// Store path: each audio block becomes its own `Transcript` carrying
    /// the retained-audio handle, preserving block order.
    async fn retain_and_transcribe(
        &self,
        original: Vec<ContentBlock>,
        model: &SttModelId,
        store: &Arc<dyn AudioStore>,
        subject: &Subject,
    ) -> Result<Vec<ContentBlock>, ChannelError> {
        let mut out = Vec::with_capacity(original.len());
        for block in original {
            match block {
                ContentBlock::Audio(payload) => {
                    out.push(self.retain_one(payload, model, store, subject).await?);
                }
                other => out.push(other),
            }
        }
        Ok(out)
    }

    async fn retain_one(
        &self,
        payload: AudioPayload,
        model: &SttModelId,
        store: &Arc<dyn AudioStore>,
        subject: &Subject,
    ) -> Result<ContentBlock, ChannelError> {
        let audio_bytes = bytes::Bytes::copy_from_slice(payload.bytes());
        let mime = payload.mime().to_owned();
        let response = self
            .stt
            .transcribe(SttRequest {
                payload: payload.clone(),
                model: model.clone(),
                language: None,
            })
            .await
            .map_err(|error| stt_failed(&error))?;

        let voice = self
            .voice_signals(payload, response.clone(), subject.clone())
            .await;
        match store.put(audio_bytes, &mime).await {
            Ok(source_audio) => Ok(ContentBlock::Transcript {
                text: response.text,
                source_audio,
                voice,
            }),
            Err(error) => {
                crabgent_log::warn!(
                    %error,
                    "audio store put failed; keeping transcript without retained audio"
                );
                Ok(ContentBlock::Text {
                    text: format!("{TRANSCRIPT_PREFIX}{}", response.text),
                })
            }
        }
    }

    async fn voice_signals(
        &self,
        payload: AudioPayload,
        response: SttResponse,
        subject: Subject,
    ) -> Option<crabgent_core::VoiceSignals> {
        let base = crabgent_prosody::voice_signals(&response, &self.prosody);
        let identities = self
            .identify_speaker(payload, response, subject)
            .await
            .unwrap_or_default();
        merge_speaker_identities(base, identities)
    }

    async fn identify_speaker(
        &self,
        payload: AudioPayload,
        transcription: SttResponse,
        subject: Subject,
    ) -> Option<Vec<SpeakerIdentity>> {
        let identifier = self.speaker_identifier.as_ref()?;
        match identifier
            .identify(SpeakerIdentificationRequest {
                payload,
                transcription,
                subject,
            })
            .await
        {
            Ok(identities) => Some(identities),
            Err(error) => {
                crabgent_log::warn!(%error, "speaker identification failed; keeping transcript");
                None
            }
        }
    }

    /// No-store path: collapse all transcripts into a single prefixed text
    /// block at the first audio position (legacy, fail-closed retention).
    async fn collapse_transcribe(
        &self,
        original: Vec<ContentBlock>,
        model: &SttModelId,
    ) -> Result<Vec<ContentBlock>, ChannelError> {
        let mut attachments = Vec::with_capacity(original.len());
        let mut transcripts = Vec::new();
        let mut transcript_index = None;

        for block in original {
            match block {
                ContentBlock::Audio(payload) => {
                    transcript_index.get_or_insert(attachments.len());
                    let response = self
                        .stt
                        .transcribe(SttRequest {
                            payload,
                            model: model.clone(),
                            language: None,
                        })
                        .await
                        .map_err(|error| stt_failed(&error))?;
                    transcripts.push(format!("{TRANSCRIPT_PREFIX}{}", response.text));
                }
                other => attachments.push(other),
            }
        }

        if let Some(index) = transcript_index {
            attachments.insert(
                index,
                ContentBlock::Text {
                    text: transcripts.join("\n"),
                },
            );
        }
        Ok(attachments)
    }

    fn default_model(&self) -> Result<SttModelId, ChannelError> {
        self.stt
            .models()
            .into_iter()
            .next()
            .map(|info| info.id)
            .ok_or_else(|| stt_failed(&SttError::ModelUnknown))
    }
}

fn stt_failed(error: &SttError) -> ChannelError {
    ChannelError::SttFailed(error.to_string())
}

fn merge_speaker_identities(
    base: Option<crabgent_core::VoiceSignals>,
    identities: Vec<SpeakerIdentity>,
) -> Option<crabgent_core::VoiceSignals> {
    if identities.is_empty() {
        return base;
    }
    let mut voice = base.unwrap_or_default();
    voice.speaker_identities = identities;
    Some(voice)
}

/// Log a fail-closed retention skip for the per-subject audit trail. The single
/// `warn!` site keeps `retention_allowed` under the cognitive-complexity cap:
/// the macro expands into several branches under workspace feature unification,
/// so two inline sites breach 15. The policy reason is owned by the implementor
/// and not surfaced here.
fn warn_retention_skipped(subject: &str, reason: &str) {
    crabgent_log::warn!(
        subject,
        reason,
        "audio retention skipped; keeping transcript only"
    );
}

#[cfg(any(test, feature = "test-helpers"))]
pub mod test_helpers {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use crabgent_core::{
        SttError, SttEventStream, SttModelId, SttModelInfo, SttProvider, SttProviderCapabilities,
        SttRequest, SttResponse,
    };

    /// Test STT provider with scripted responses.
    pub struct MockSttProvider {
        responses: Vec<String>,
        error: Option<SttError>,
        calls: AtomicUsize,
    }

    impl MockSttProvider {
        pub const fn with_responses(responses: Vec<String>) -> Self {
            Self {
                responses,
                error: None,
                calls: AtomicUsize::new(0),
            }
        }

        pub const fn with_error(error: SttError) -> Self {
            Self {
                responses: Vec::new(),
                error: Some(error),
                calls: AtomicUsize::new(0),
            }
        }

        pub fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl SttProvider for MockSttProvider {
        async fn transcribe(&self, req: SttRequest) -> Result<SttResponse, SttError> {
            let idx = self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(error) = self.error.clone() {
                return Err(error);
            }

            let text = self
                .responses
                .get(idx)
                .or_else(|| self.responses.last())
                .cloned()
                .unwrap_or_default();
            Ok(SttResponse {
                text,
                model: req.model,
                segments: Vec::new(),
                audio_events: Vec::new(),
                language: None,
            })
        }

        async fn stream(&self, _req: SttRequest) -> Result<SttEventStream, SttError> {
            Err(SttError::Backend("mock streaming unsupported".into()))
        }

        fn capabilities(&self) -> SttProviderCapabilities {
            SttProviderCapabilities {
                streaming: false,
                audio: true,
            }
        }

        fn models(&self) -> Vec<SttModelInfo> {
            vec![SttModelInfo {
                id: SttModelId::new("mock-stt"),
                supports_streaming: false,
                supports_diarization: false,
            }]
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crabgent_core::{AudioPayload, SttProvider, SttRequest};

    use crate::stt_inbox::test_helpers::MockSttProvider;

    #[tokio::test]
    async fn mock_provider_via_test_helpers_feature() {
        let provider = Arc::new(MockSttProvider::with_responses(vec!["hello".into()]));
        let model = provider.models().remove(0).id;
        let response = provider
            .transcribe(SttRequest {
                payload: AudioPayload::new(vec![b'a'], "audio/ogg", None)
                    .expect("valid audio payload"),
                model,
                language: None,
            })
            .await
            .expect("mock response");

        assert_eq!(response.text, "hello");
        assert_eq!(provider.call_count(), 1);
    }
}
