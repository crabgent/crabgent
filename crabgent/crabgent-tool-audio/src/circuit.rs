//! Circuit breaker + per-call budget for the shared `[audio]` route.
//!
//! Both audio callers (the [`crate::HearAgainTool`] pull and the divergence
//! divergence push) route their [`ask_audio`](crate::ask_audio) call through one
//! shared [`AudioCircuit`] so a degraded audio provider cannot fan latency and
//! cost across sessions. The circuit enforces three bounds:
//!
//! - a per-call wall-clock timeout (latency ceiling per call),
//! - a consecutive-failure breaker that trips the path off after N transport
//!   failures and stays open for a cooldown (cost ceiling across calls),
//! - a max-send-bytes ceiling applied inside `ask_audio` (cost ceiling per
//!   call, on top of the existing output-token/byte caps).
//!
//! The breaker is process-global (one shared `Arc<AudioCircuit>`), which is
//! stronger than per-conversation for the stated goal: a degraded provider
//! trips the path off everywhere, not once per conversation. Tripping is logged
//! (operator-visible), never silent. Every caller treats a tripped breaker as
//! fail-open: the turn completes on the plain transcript.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::time::{Instant, timeout};

use crate::call::{AskAudioError, AudioAnswer};

/// Tunables for [`AudioCircuit`].
#[derive(Debug, Clone)]
pub struct AudioCircuitConfig {
    /// Consecutive transport failures before the breaker trips.
    pub max_consecutive_failures: u32,
    /// Wall-clock ceiling for one audio call.
    pub per_call_timeout: Duration,
    /// How long the breaker stays open before a half-open probe is allowed.
    pub cooldown: Duration,
    /// Largest retained clip (bytes) sent to the audio model.
    pub max_send_bytes: usize,
}

impl Default for AudioCircuitConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 3,
            per_call_timeout: Duration::from_secs(5),
            cooldown: Duration::from_secs(30),
            max_send_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Shared breaker guarding the `[audio]` route.
pub struct AudioCircuit {
    cfg: AudioCircuitConfig,
    consecutive_failures: AtomicU32,
    open_until: Mutex<Option<Instant>>,
}

impl AudioCircuit {
    /// Build a circuit from explicit tunables.
    #[must_use]
    pub const fn new(cfg: AudioCircuitConfig) -> Self {
        Self {
            cfg,
            consecutive_failures: AtomicU32::new(0),
            open_until: Mutex::new(None),
        }
    }

    /// The per-call byte ceiling, passed into `ask_audio` so an oversized clip
    /// fails open before the provider call.
    #[must_use]
    pub const fn max_send_bytes(&self) -> usize {
        self.cfg.max_send_bytes
    }

    /// `true` while the breaker is tripped and its cooldown has not elapsed.
    #[must_use]
    pub fn is_open(&self) -> bool {
        let guard = self
            .open_until
            .lock()
            .expect("audio circuit mutex poisoned");
        guard.is_some_and(|until| Instant::now() < until)
    }

    /// Run one audio call under the breaker + timeout.
    ///
    /// Short-circuits to [`AskAudioError::CircuitOpen`] without polling `fut`
    /// when the breaker is open. Otherwise wraps `fut` in the per-call timeout;
    /// a timeout or a transport failure ([`AskAudioError::is_transport_failure`])
    /// counts toward the trip, every other outcome leaves the counter
    /// untouched, and a success resets it.
    pub async fn run<F>(&self, fut: F) -> Result<AudioAnswer, AskAudioError>
    where
        F: Future<Output = Result<AudioAnswer, AskAudioError>>,
    {
        if self.is_open() {
            return Err(AskAudioError::CircuitOpen);
        }
        match timeout(self.cfg.per_call_timeout, fut).await {
            Err(_elapsed) => {
                self.record_failure();
                Err(AskAudioError::Timeout)
            }
            Ok(Ok(answer)) => {
                self.record_success();
                Ok(answer)
            }
            Ok(Err(err)) => {
                if err.is_transport_failure() {
                    self.record_failure();
                }
                Err(err)
            }
        }
    }

    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        *self
            .open_until
            .lock()
            .expect("audio circuit mutex poisoned") = None;
    }

    fn record_failure(&self) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        if failures < self.cfg.max_consecutive_failures {
            return;
        }
        let mut guard = self
            .open_until
            .lock()
            .expect("audio circuit mutex poisoned");
        let already_open = guard.is_some_and(|until| Instant::now() < until);
        *guard = Some(Instant::now() + self.cfg.cooldown);
        if !already_open {
            crabgent_log::warn!(
                failures,
                "audio circuit breaker tripped; pausing the audio-perception path"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use crabgent_core::ModelId;

    use super::{AskAudioError, AudioAnswer, AudioCircuit, AudioCircuitConfig};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    fn answer() -> AudioAnswer {
        AudioAnswer {
            answer: "calm".to_owned(),
            model: ModelId::new("audio-model"),
        }
    }

    fn cfg(threshold: u32, cooldown: Duration) -> AudioCircuitConfig {
        AudioCircuitConfig {
            max_consecutive_failures: threshold,
            per_call_timeout: Duration::from_secs(5),
            cooldown,
            max_send_bytes: 1024,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn trips_after_threshold_then_short_circuits() {
        let circuit = AudioCircuit::new(cfg(3, Duration::from_secs(30)));
        for _ in 0..3 {
            let result = circuit
                .run(async {
                    // Outlives the 5s per-call timeout; paused time auto-advances
                    // to the earliest timer (the timeout) and yields Elapsed.
                    tokio::time::sleep(Duration::from_mins(1)).await;
                    Ok(answer())
                })
                .await;
            assert!(matches!(result, Err(AskAudioError::Timeout)));
        }
        assert!(circuit.is_open(), "breaker trips after the threshold");

        let ran = Arc::new(AtomicBool::new(false));
        let probe = Arc::clone(&ran);
        let result = circuit
            .run(async move {
                probe.store(true, Ordering::SeqCst);
                Ok(answer())
            })
            .await;
        assert!(matches!(result, Err(AskAudioError::CircuitOpen)));
        assert!(
            !ran.load(Ordering::SeqCst),
            "an open breaker must not run the call"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn recovers_after_cooldown_on_success() {
        let circuit = AudioCircuit::new(cfg(1, Duration::from_secs(30)));
        let tripped = circuit
            .run(async {
                tokio::time::sleep(Duration::from_mins(1)).await;
                Ok(answer())
            })
            .await;
        assert!(matches!(tripped, Err(AskAudioError::Timeout)));
        assert!(circuit.is_open());

        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(!circuit.is_open(), "cooldown elapsed: half-open");

        let recovered = circuit.run(async { Ok(answer()) }).await;
        assert!(recovered.is_ok(), "successful probe closes the breaker");
        assert!(!circuit.is_open());
    }

    #[tokio::test(start_paused = true)]
    async fn re_trips_when_probe_fails_after_cooldown() {
        let circuit = AudioCircuit::new(cfg(1, Duration::from_secs(30)));
        let tripped = circuit
            .run(async {
                tokio::time::sleep(Duration::from_mins(1)).await;
                Ok(answer())
            })
            .await;
        assert!(matches!(tripped, Err(AskAudioError::Timeout)));
        assert!(circuit.is_open());

        // Cooldown elapses: half-open, one probe allowed.
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(!circuit.is_open());

        // The probe fails again: the breaker re-arms with a fresh cooldown.
        let probe = circuit
            .run(async {
                tokio::time::sleep(Duration::from_mins(1)).await;
                Ok(answer())
            })
            .await;
        assert!(matches!(probe, Err(AskAudioError::Timeout)));
        assert!(circuit.is_open(), "a failed probe re-trips the breaker");

        // A subsequent call short-circuits without running the future.
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        let result = circuit
            .run(async move {
                flag.store(true, Ordering::SeqCst);
                Ok(answer())
            })
            .await;
        assert!(matches!(result, Err(AskAudioError::CircuitOpen)));
        assert!(
            !ran.load(Ordering::SeqCst),
            "the re-tripped breaker does not run the call"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn non_transport_failures_do_not_trip() {
        let circuit = AudioCircuit::new(cfg(2, Duration::from_secs(30)));
        for _ in 0..5 {
            let result = circuit.run(async { Err(AskAudioError::NotFound) }).await;
            assert!(matches!(result, Err(AskAudioError::NotFound)));
        }
        assert!(
            !circuit.is_open(),
            "NotFound is not provider degradation and must not trip the breaker"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn success_resets_the_failure_run() {
        let circuit = AudioCircuit::new(cfg(3, Duration::from_secs(30)));
        for _ in 0..2 {
            let result = circuit
                .run(async {
                    tokio::time::sleep(Duration::from_mins(1)).await;
                    Ok(answer())
                })
                .await;
            assert!(matches!(result, Err(AskAudioError::Timeout)));
        }
        assert!(!circuit.is_open(), "two failures stay under the threshold");
        circuit
            .run(async { Ok(answer()) })
            .await
            .expect("probe succeeds and resets the counter");
        // After a reset, two more failures must not trip (counter restarted).
        for _ in 0..2 {
            let result = circuit
                .run(async {
                    tokio::time::sleep(Duration::from_mins(1)).await;
                    Ok(answer())
                })
                .await;
            assert!(matches!(result, Err(AskAudioError::Timeout)));
        }
        assert!(!circuit.is_open());
    }
}
