//! Trait-default `ChannelInbox::shutdown` behaviour and the
//! `Arc<I>`-blanket forwarding contract.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use crabgent_channel::{ChannelError, ChannelInbox, InboundEvent};

struct Dummy;

#[async_trait]
impl ChannelInbox for Dummy {
    async fn receive(&self, _event: InboundEvent) -> Result<(), ChannelError> {
        Ok(())
    }
}

struct Counter {
    c: Arc<AtomicUsize>,
}

#[async_trait]
impl ChannelInbox for Counter {
    async fn receive(&self, _event: InboundEvent) -> Result<(), ChannelError> {
        Ok(())
    }

    async fn shutdown(&self, _grace: Duration) {
        self.c.fetch_add(1, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn trait_default_shutdown_is_noop() {
    let inbox = Dummy;
    let start = Instant::now();
    inbox.shutdown(Duration::from_millis(100)).await;
    assert!(start.elapsed() < Duration::from_millis(50));
}

#[tokio::test]
async fn arc_blanket_forwards_shutdown() {
    let counter = Arc::new(AtomicUsize::new(0));
    let arc_inbox: Arc<dyn ChannelInbox> = Arc::new(Counter { c: counter.clone() });
    arc_inbox.shutdown(Duration::ZERO).await;
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn stt_inbox_forwards_shutdown_to_inner() {
    use crabgent_channel::stt_inbox::SttInbox;
    use crabgent_core::{
        SttError, SttEventStream, SttProvider, SttProviderCapabilities, SttRequest, SttResponse,
    };

    struct NoopStt;
    #[async_trait]
    impl SttProvider for NoopStt {
        async fn transcribe(&self, _req: SttRequest) -> Result<SttResponse, SttError> {
            Err(SttError::Backend(
                "transcribe not called in shutdown test".to_owned(),
            ))
        }
        async fn stream(&self, _req: SttRequest) -> Result<SttEventStream, SttError> {
            Err(SttError::Backend(
                "stream not called in shutdown test".to_owned(),
            ))
        }
        fn capabilities(&self) -> SttProviderCapabilities {
            SttProviderCapabilities::default()
        }
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let inner = Counter { c: counter.clone() };
    let stt: Arc<dyn SttProvider> = Arc::new(NoopStt);
    let inbox = SttInbox::new(stt, inner);
    inbox.shutdown(Duration::ZERO).await;
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn recording_inbox_forwards_shutdown_to_inner() {
    use crabgent_channel::recording_inbox::{InboundRecorder, RecordDecision, RecordingInbox};

    struct PassthroughRecorder;
    #[async_trait]
    impl InboundRecorder for PassthroughRecorder {
        async fn record(&self, _event: &InboundEvent) -> Result<RecordDecision, ChannelError> {
            Ok(RecordDecision::Forward)
        }
    }

    let counter = Arc::new(AtomicUsize::new(0));
    let inner = Counter { c: counter.clone() };
    let inbox = RecordingInbox::new(
        Arc::new(PassthroughRecorder) as Arc<dyn InboundRecorder>,
        inner,
    );
    inbox.shutdown(Duration::ZERO).await;
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn startup_cutoff_inbox_forwards_shutdown_to_inner() {
    use crabgent_channel::startup_cutoff_inbox::StartupCutoffInbox;

    let counter = Arc::new(AtomicUsize::new(0));
    let inner = Arc::new(Counter { c: counter.clone() }) as Arc<dyn ChannelInbox>;
    let inbox = StartupCutoffInbox::new(inner);
    inbox.shutdown(Duration::ZERO).await;
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn pairing_inbox_forwards_shutdown_to_inner() {
    use crabgent_channel::{
        ChannelRouter, ChannelSink, MemoryPairingStore, PairingInbox, PairingStore,
    };

    let counter = Arc::new(AtomicUsize::new(0));
    let inner = Arc::new(Counter { c: counter.clone() }) as Arc<dyn ChannelInbox>;
    let store: Arc<dyn PairingStore> = Arc::new(MemoryPairingStore::default());
    let router: Arc<dyn ChannelSink> = Arc::new(ChannelRouter::new());
    let inbox = PairingInbox::new(store, inner, router, "token");
    inbox.shutdown(Duration::ZERO).await;
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}
