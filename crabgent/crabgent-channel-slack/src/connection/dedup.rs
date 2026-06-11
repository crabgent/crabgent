//! Envelope deduplication shared across Socket Mode connections.
//!
//! Slack delivers each Socket Mode envelope to every open connection in the
//! delivery pool. Running more than one connection therefore yields duplicate
//! envelopes that must be discarded before dispatch. [`EnvelopeDedup`] keeps a
//! bounded FIFO set of recently seen `envelope_id`s for that purpose. It holds
//! no Slack tokens or payload bodies, only the opaque envelope identifiers.

use std::collections::{HashSet, VecDeque};
use std::sync::{Mutex, PoisonError};

/// Number of `envelope_id`s retained for duplicate detection.
const DEFAULT_CAPACITY: usize = 2048;

/// Bounded, thread-safe deduplicator keyed on `envelope_id`.
///
/// Shared via `Arc` across every connection in a `SocketModePool` so a
/// redelivery on a sibling connection is recognised as a duplicate.
#[derive(Debug)]
pub struct EnvelopeDedup {
    capacity: usize,
    state: Mutex<DedupState>,
}

#[derive(Debug, Default)]
struct DedupState {
    seen: HashSet<String>,
    order: VecDeque<String>,
}

impl Default for EnvelopeDedup {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvelopeDedup {
    /// Create a deduplicator retaining the default window of 2048 ids.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Create a deduplicator retaining at most `capacity` ids (minimum 1).
    fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            state: Mutex::new(DedupState::default()),
        }
    }

    /// Record `envelope_id` and report whether it is new.
    ///
    /// Returns `true` the first time an id is seen (and records it) and `false`
    /// for any later occurrence while the id is still inside the retained
    /// window. The oldest id is evicted once the window exceeds the configured
    /// capacity, so a sufficiently old id can be accepted again.
    pub fn accept(&self, envelope_id: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.seen.contains(envelope_id) {
            return false;
        }
        let id = envelope_id.to_owned();
        state.seen.insert(id.clone());
        state.order.push_back(id);
        if state.order.len() > self.capacity
            && let Some(evicted) = state.order.pop_front()
        {
            state.seen.remove(&evicted);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::EnvelopeDedup;

    #[test]
    fn same_id_accepted_only_once() {
        let dedup = EnvelopeDedup::new();
        assert!(dedup.accept("E1"), "first sight is new");
        assert!(!dedup.accept("E1"), "second sight is a duplicate");
    }

    #[test]
    fn distinct_ids_are_each_accepted() {
        let dedup = EnvelopeDedup::new();
        assert!(dedup.accept("E1"));
        assert!(dedup.accept("E2"));
        assert!(dedup.accept("E3"));
    }

    #[test]
    fn window_stays_bounded_and_evicts_oldest() {
        let dedup = EnvelopeDedup::with_capacity(2);
        assert!(dedup.accept("a"));
        assert!(dedup.accept("b"));
        assert!(dedup.accept("c"), "third id evicts the oldest (a)");
        assert!(!dedup.accept("b"), "b is still inside the window");
        assert!(!dedup.accept("c"), "c is still inside the window");
        assert!(dedup.accept("a"), "a was evicted, so it is new again");
    }
}
