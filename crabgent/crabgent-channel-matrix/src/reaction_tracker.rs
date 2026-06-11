//! Bounded in-memory tracker for inbound `m.reaction` events.
//!
//! Matrix delivers `m.reaction` removals as `m.room.redaction` events
//! referencing the original reaction `event_id`. The redaction itself
//! carries neither the reacted-to message nor the emoji, so the
//! adapter retains a short FIFO cache mapping each reaction
//! `event_id` to the data needed to synthesise an
//! `InboundReaction { added: false }` when a redaction arrives.
//!
//! The cache is capacity-bounded; reactions older than the cap window
//! get silently dropped on redaction. Process restart clears state,
//! so redactions of pre-restart reactions are also silently ignored.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId, OwnedUserId};

/// Default ring size for the reaction cache.
pub const DEFAULT_CAPACITY: usize = 1024;

/// Data retained per observed reaction so a later redaction can
/// reconstruct an `InboundReaction { added: false }` event.
#[derive(Debug, Clone)]
pub struct TrackedReaction {
    pub target_event_id: OwnedEventId,
    pub key: String,
    pub sender: OwnedUserId,
    pub room_id: OwnedRoomId,
}

/// FIFO cache mapping reaction `event_id` -> [`TrackedReaction`].
#[derive(Debug)]
pub struct ReactionTracker {
    inner: Mutex<Inner>,
    capacity: usize,
}

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<OwnedEventId, TrackedReaction>,
    order: VecDeque<OwnedEventId>,
}

impl Default for ReactionTracker {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }
}

impl ReactionTracker {
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            capacity: capacity.max(1),
        }
    }

    /// Insert a reaction. Evicts the oldest entry when the cap fills.
    pub fn record(&self, reaction_event_id: OwnedEventId, entry: TrackedReaction) {
        let mut inner = self.inner.lock().expect("reaction tracker poisoned");
        if let Some(slot) = inner.map.get_mut(&reaction_event_id) {
            *slot = entry;
            return;
        }
        if inner.order.len() >= self.capacity
            && let Some(oldest) = inner.order.pop_front()
        {
            inner.map.remove(&oldest);
        }
        inner.order.push_back(reaction_event_id.clone());
        inner.map.insert(reaction_event_id, entry);
    }

    /// Single-shot lookup: remove and return the tracked reaction.
    pub fn take(&self, reaction_event_id: &OwnedEventId) -> Option<TrackedReaction> {
        let mut inner = self.inner.lock().expect("reaction tracker poisoned");
        let entry = inner.map.remove(reaction_event_id)?;
        inner.order.retain(|id| id != reaction_event_id);
        Some(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::{owned_event_id, owned_room_id, owned_user_id};

    fn dummy(target: OwnedEventId, key: &str) -> TrackedReaction {
        TrackedReaction {
            target_event_id: target,
            key: key.to_owned(),
            sender: owned_user_id!("@a:server"),
            room_id: owned_room_id!("!r:server"),
        }
    }

    #[test]
    fn record_and_take_round_trip() {
        let t = ReactionTracker::default();
        let id = owned_event_id!("$reaction:server");
        t.record(id.clone(), dummy(owned_event_id!("$target:server"), "+1"));
        let got = t.take(&id).expect("present");
        assert_eq!(got.key, "+1");
        assert!(t.take(&id).is_none(), "take is single-shot");
    }

    #[test]
    fn capacity_evicts_oldest() {
        let t = ReactionTracker::with_capacity(2);
        let r1 = owned_event_id!("$r1:server");
        let r2 = owned_event_id!("$r2:server");
        let r3 = owned_event_id!("$r3:server");
        t.record(r1.clone(), dummy(owned_event_id!("$t1:server"), "a"));
        t.record(r2.clone(), dummy(owned_event_id!("$t2:server"), "b"));
        t.record(r3.clone(), dummy(owned_event_id!("$t3:server"), "c"));
        assert!(t.take(&r1).is_none(), "oldest must evict at cap");
        assert!(t.take(&r2).is_some());
        assert!(t.take(&r3).is_some());
    }

    #[test]
    fn zero_capacity_clamps_to_one() {
        let t = ReactionTracker::with_capacity(0);
        let r1 = owned_event_id!("$r1:server");
        let r2 = owned_event_id!("$r2:server");
        t.record(r1.clone(), dummy(owned_event_id!("$t1:server"), "a"));
        t.record(r2.clone(), dummy(owned_event_id!("$t2:server"), "b"));
        assert!(t.take(&r1).is_none(), "evicted when cap clamps to 1");
        assert!(t.take(&r2).is_some());
    }

    #[test]
    fn duplicate_record_overwrites_in_place() {
        let t = ReactionTracker::with_capacity(2);
        let id = owned_event_id!("$dup:server");
        t.record(id.clone(), dummy(owned_event_id!("$t1:server"), "old"));
        t.record(id.clone(), dummy(owned_event_id!("$t2:server"), "new"));
        let got = t.take(&id).expect("present");
        assert_eq!(got.key, "new");
        assert_eq!(got.target_event_id.as_str(), "$t2:server");
    }
}
