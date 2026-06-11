//! Pre-warmed readable Slack conversation labels.
//!
//! Built once at startup by [`crate::inbox::SlackInbox::pre_warm_channel_names`]
//! and shared into [`crate::channel::SlackChannel`] so its
//! [`crabgent_channel::Channel::conv_display`] resolves names from a local map
//! rather than a network round-trip on the dispatch hot-path. The user decided
//! against a lazy/TTL cache: this is a one-shot pre-warm, so channels created
//! after startup are simply absent (the tag omits the name, fail-soft).

use std::collections::HashMap;
use std::sync::Arc;

use crate::ids::SlackChannelId;

/// Readable Slack channel names plus the constant-per-connection workspace.
///
/// Cheap to clone: the name map is behind an `Arc`. An empty map (e.g. the
/// listing scope was not granted) is valid and yields no `name` labels.
#[derive(Debug, Clone, Default)]
pub struct SlackChannelNames {
    names: Arc<HashMap<SlackChannelId, String>>,
    workspace: Option<String>,
}

impl SlackChannelNames {
    /// Build from a resolved name map and the workspace label.
    #[must_use]
    pub fn new(names: HashMap<SlackChannelId, String>, workspace: Option<String>) -> Self {
        Self {
            names: Arc::new(names),
            workspace,
        }
    }

    /// Readable name for `channel`, when the pre-warm captured it.
    #[must_use]
    pub fn name(&self, channel: &SlackChannelId) -> Option<&str> {
        self.names.get(channel).map(String::as_str)
    }

    /// Constant-per-connection workspace/team label, when resolved.
    #[must_use]
    pub fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    /// Number of channels captured by the pre-warm.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// `true` when no channel names were captured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_hits_and_misses() {
        let mut map = HashMap::new();
        map.insert(
            SlackChannelId::new("C1").expect("id"),
            "platform-ops".to_owned(),
        );
        let names = SlackChannelNames::new(map, Some("example".to_owned()));

        assert_eq!(
            names.name(&SlackChannelId::new("C1").expect("id")),
            Some("platform-ops")
        );
        assert_eq!(names.name(&SlackChannelId::new("C9").expect("id")), None);
        assert_eq!(names.workspace(), Some("example"));
        assert_eq!(names.len(), 1);
        assert!(!names.is_empty());
    }

    #[test]
    fn default_is_empty_with_no_workspace() {
        let names = SlackChannelNames::default();
        assert!(names.is_empty());
        assert_eq!(names.len(), 0);
        assert_eq!(names.workspace(), None);
    }
}
