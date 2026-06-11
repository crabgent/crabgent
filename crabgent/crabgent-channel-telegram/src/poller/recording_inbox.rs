//! Shared recording inbox for `mod tests_audio` and `mod tests_sanitize`.
//!
//! Both modules drive `tick_once` against a mock Telegram API and capture the
//! events the poller hands to its inbox. They share this minimal recording
//! inbox; the per-module poller builders stay local because they diverge on
//! audio support.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crabgent_channel::{ChannelError, ChannelInbox, InboundEvent};

/// A [`ChannelInbox`] that records received events for later draining.
pub(super) struct RecordingInbox {
    events: Mutex<Vec<InboundEvent>>,
}

impl RecordingInbox {
    pub(super) const fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    /// Take and clear every recorded event.
    pub(super) fn drain(&self) -> Vec<InboundEvent> {
        std::mem::take(&mut *self.events.lock().expect("mutex should not be poisoned"))
    }
}

#[async_trait]
impl ChannelInbox for RecordingInbox {
    async fn receive(&self, event: InboundEvent) -> Result<(), ChannelError> {
        self.events
            .lock()
            .expect("mutex should not be poisoned")
            .push(event);
        Ok(())
    }
}

/// Erase a concrete [`RecordingInbox`] handle to the inbox trait object the
/// poller builders accept.
pub(super) fn inbox_obj(inbox: &Arc<RecordingInbox>) -> Arc<dyn ChannelInbox> {
    inbox.clone()
}
