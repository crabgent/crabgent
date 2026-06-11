//! Periodic scheduler that evicts expired retained audio.
//!
//! [`AudioStoreSweeper`] turns the on-demand [`AudioStore::sweep_expired`]
//! into a bounded background task: without it a store grows until the disk
//! fills. The consumer owns the lifecycle (spawn + cancel); the channel crate
//! ships only the loop. Scheduling lives here rather than in the consumer so
//! the cadence + cancellation are testable in one place.

use std::sync::Arc;
use std::time::Duration;

use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use super::{AudioStore, AudioStoreError};

/// Background task that calls [`AudioStore::sweep_expired`] on a fixed
/// interval until cancelled.
pub struct AudioStoreSweeper {
    store: Arc<dyn AudioStore>,
    ttl: Duration,
    every: Duration,
}

impl AudioStoreSweeper {
    /// Build a sweeper that evicts audio older than `ttl` every `every`.
    #[must_use]
    pub const fn new(store: Arc<dyn AudioStore>, ttl: Duration, every: Duration) -> Self {
        Self { store, ttl, every }
    }

    /// Run the sweep loop until `cancel` fires.
    ///
    /// The first sweep runs immediately, then once per `every`. A slow sweep
    /// never bursts catch-up ticks ([`MissedTickBehavior::Skip`]). Sweep
    /// errors are logged and the loop continues; the task only exits on
    /// cancellation.
    pub async fn run(self, cancel: CancellationToken) {
        let mut tick = interval(self.every);
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick.tick() => self.sweep_once().await,
                () = cancel.cancelled() => break,
            }
        }
    }

    async fn sweep_once(&self) {
        match self.store.sweep_expired(self.ttl).await {
            Ok(0) => {}
            Ok(removed) => log_swept(removed),
            Err(error) => log_sweep_failed(&error),
        }
    }
}

// One tracing macro per helper: keeping both out of `sweep_once` holds it under
// the cognitive-complexity cap (the macro expands into several branches under
// workspace feature unification, so two inline sites breach 15).
fn log_swept(removed: usize) {
    crabgent_log::info!(removed, "audio store sweep evicted expired files");
}

fn log_sweep_failed(error: &AudioStoreError) {
    crabgent_log::warn!(%error, "audio store sweep failed");
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use crabgent_core::AudioRef;

    use super::super::{AudioStore, AudioStoreError};
    use super::{AudioStoreSweeper, CancellationToken, Duration};

    #[derive(Default)]
    struct CountingStore {
        sweeps: AtomicUsize,
    }

    impl CountingStore {
        fn sweeps(&self) -> usize {
            self.sweeps.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl AudioStore for CountingStore {
        async fn put(
            &self,
            _bytes: bytes::Bytes,
            _mime: &str,
        ) -> Result<AudioRef, AudioStoreError> {
            Err(AudioStoreError::NotFound)
        }

        async fn get(
            &self,
            _audio_ref: &AudioRef,
        ) -> Result<(bytes::Bytes, String), AudioStoreError> {
            Err(AudioStoreError::NotFound)
        }

        async fn sweep_expired(&self, _ttl: Duration) -> Result<usize, AudioStoreError> {
            self.sweeps.fetch_add(1, Ordering::SeqCst);
            Ok(0)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn sweeps_on_each_tick_and_stops_on_cancel() {
        let store = std::sync::Arc::new(CountingStore::default());
        let cancel = CancellationToken::new();
        let sweeper = AudioStoreSweeper::new(
            store.clone(),
            Duration::from_hours(24),
            Duration::from_mins(1),
        );
        let handle = tokio::spawn(sweeper.run(cancel.clone()));

        // First tick is immediate; advance three interval periods.
        for _ in 0..3 {
            tokio::time::advance(Duration::from_mins(1)).await;
            tokio::task::yield_now().await;
        }

        cancel.cancel();
        // The loop exits on cancel; if it did not, this await would hang.
        handle.await.expect("sweeper task joins after cancel");

        assert!(
            store.sweeps() >= 3,
            "expected repeated sweeps, got {}",
            store.sweeps()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn default_trait_sweep_is_noop() {
        // A store without an expiry override sweeps to a no-op without error.
        struct NoExpiryStore;

        #[async_trait]
        impl AudioStore for NoExpiryStore {
            async fn put(
                &self,
                _bytes: bytes::Bytes,
                _mime: &str,
            ) -> Result<AudioRef, AudioStoreError> {
                Err(AudioStoreError::NotFound)
            }

            async fn get(
                &self,
                _audio_ref: &AudioRef,
            ) -> Result<(bytes::Bytes, String), AudioStoreError> {
                Err(AudioStoreError::NotFound)
            }
        }

        let removed = NoExpiryStore
            .sweep_expired(Duration::from_secs(1))
            .await
            .expect("default sweep is infallible");
        assert_eq!(removed, 0);
    }
}
