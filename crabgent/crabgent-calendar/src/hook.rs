//! [`Hook`] implementation that injects time and holiday context into the LLM
//! system prompt before each call.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crabgent_core::{Decision, Hook, LlmRequest, RunCtx};

use crate::config::{Clock, TimeHintConfig};
use crate::hint_format::build_hint;
use crate::provider::HolidayProvider;

/// Open marker of the injected time-hint block. The `crabgent="1"` sentinel
/// brings `<time>` in line with the other privileged hook tags
/// (`<voice crabgent="1">`, `<perception crabgent="1">`,
/// `<thread_goal crabgent="1">`): a kernel-injected trusted tag is sentinel-
/// anchored so its origin is unambiguous in the trust fence.
///
/// No separate user-message defang is needed here, unlike those hooks. The
/// `<time>` block is injected ONLY into the operator-controlled system prompt
/// (`build_hint` -> [`strip_old_hint`], which edits `system_prompt` exclusively;
/// [`crate::annotate::annotate_recent_user_messages`] only prepends a bracketed
/// `[ts, ago]` prefix to user text, never a tag). A literal forged
/// `<time crabgent="1"` typed by a user reaches the LLM through the inbound fence
/// (`crabgent-channel` inbox `<inbound>` wrap, `crabgent_core::sanitize::n` /
/// `xml_escape_body`), which escapes every `<`/`>` in the body to `&lt;`/`&gt;`
/// before the model sees it. The sentinel records the consistency rationale; the
/// inbound fence covers the forgery case.
pub const TIME_HINT_OPEN: &str = "<time crabgent=\"1\">";
pub const TIME_HINT_CLOSE: &str = "</time>";
pub const TIME_HINT_MARKER: &str = TIME_HINT_OPEN;
pub const TIME_HINT_CLOSE_MARKER: &str = TIME_HINT_CLOSE;
/// Sentinel-less open marker from before the trust-fence alignment. Kept in the
/// strip set so a persisted system prompt carrying the old `<time>` block is
/// still removed idempotently on the next turn (clean migration, not a runtime
/// compat shim).
const LEGACY_PLAIN_TIME_HINT_OPEN: &str = "<time>";
const LEGACY_TIME_HINT_MARKER: &str = "<!-- crabgent-calendar-time-hint -->";
const LEGACY_TIME_HINT_CLOSE_MARKER: &str = "<!-- /crabgent-calendar-time-hint -->";

/// Inline-annotation window size: how many of the most recent user
/// messages get a `[ts, X ago]` prefix on the wire-side request.
/// Older messages keep their original content so token cost stays
/// bounded as conversations grow.
pub const INLINE_ANNOTATE_LIMIT: usize = 5;

pub const TIME_GUIDANCE: &str = concat!(
    "ALWAYS assume Europe/Berlin for user-provided times unless explicitly stated otherwise.\n",
    "Resolve relative time references ('gestern', 'letzte Woche', 'vor 2 Stunden', 'yesterday', 'last Monday') to concrete dates/times based on the current datetime above. State the resolved time explicitly.\n",
    "Apply severity scaling to durations: minutes-old events are recent, hours-old events are aging, days-old events are stale.\n",
    "Weekday-date pairings come from the anchors above. NEVER state a date without matching weekday from those anchors. NEVER state a weekday without matching date.\n",
    "If the user's phrasing contradicts the anchors (e.g. 'morgen Dienstag' but tomorrow is Thursday): name the mismatch and ask which they meant. Do not silently pick one.\n",
    "Restate resolved absolute dates explicitly when relevant: 'morgen (2026-05-07, Donnerstag)'."
);

/// Hook that augments [`LlmRequest::system_prompt`] with current time anchors
/// and public holiday context.
pub struct TimeHintHook<P: HolidayProvider + 'static> {
    provider: Arc<P>,
    config: TimeHintConfig,
}

impl<P: HolidayProvider + 'static> TimeHintHook<P> {
    /// Construct a hook with default DE/NW, Europe/Berlin, and upcoming count 3.
    pub fn new(provider: Arc<P>) -> Self {
        Self {
            provider,
            config: TimeHintConfig::default(),
        }
    }

    #[must_use]
    pub fn with_config(mut self, config: TimeHintConfig) -> Self {
        self.config = config;
        self
    }

    #[must_use]
    pub fn with_country(mut self, country: impl Into<String>) -> Self {
        self.config = self.config.with_country(country);
        self
    }

    #[must_use]
    pub fn with_subdivision(mut self, subdivision: impl Into<String>) -> Self {
        self.config = self.config.with_subdivision(subdivision);
        self
    }

    #[must_use]
    pub fn with_upcoming_count(mut self, upcoming_count: usize) -> Self {
        self.config = self.config.with_upcoming_count(upcoming_count);
        self
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.config = self.config.with_clock(clock);
        self
    }
}

#[async_trait]
impl<P: HolidayProvider + 'static> Hook for TimeHintHook<P> {
    async fn before_llm(&self, req: &LlmRequest, _ctx: &RunCtx) -> Decision<LlmRequest> {
        let now = (self.config.clock)();
        let last_user_ts = last_user_timestamp(&req.messages);
        let hint = build_hint(now, &self.config, self.provider.as_ref(), last_user_ts);

        let mut next = req.clone();
        let base_prompt = next
            .system_prompt
            .as_deref()
            .map(strip_old_hint)
            .unwrap_or_default();
        next.system_prompt = Some(if base_prompt.is_empty() {
            hint
        } else {
            format!("{base_prompt}\n\n{hint}")
        });

        crate::annotate::annotate_recent_user_messages(
            &mut next.messages,
            now,
            self.config.timezone,
            INLINE_ANNOTATE_LIMIT,
        );

        Decision::Replace(next)
    }
}

/// Remove the tag-pair-bounded time-hint block from a system prompt so the
/// new hint replaces it cleanly. Legacy HTML comment markers are accepted
/// during migration, including mixed open/close pairs.
fn strip_old_hint(prompt: &str) -> String {
    let mut current = prompt.trim_end().to_owned();
    while let Some((_, open_marker)) = find_hint_open(current.as_str()) {
        let Some((head, after_open)) = current.split_once(open_marker) else {
            break;
        };
        let tail = find_hint_close(after_open)
            .and_then(|(_, close_marker)| after_open.split_once(close_marker))
            .map_or("", |(_, after_close)| after_close);
        current = format!("{}{}", head.trim_end(), tail.trim_start());
    }
    current.trim_matches(|c: char| c == '\n').to_owned()
}

fn find_hint_open(prompt: &str) -> Option<(usize, &'static str)> {
    find_earliest_marker(
        prompt,
        &[
            TIME_HINT_OPEN,
            LEGACY_PLAIN_TIME_HINT_OPEN,
            LEGACY_TIME_HINT_MARKER,
        ],
    )
}

fn find_hint_close(prompt: &str) -> Option<(usize, &'static str)> {
    find_earliest_marker(prompt, &[TIME_HINT_CLOSE, LEGACY_TIME_HINT_CLOSE_MARKER])
}

fn find_earliest_marker(prompt: &str, markers: &[&'static str]) -> Option<(usize, &'static str)> {
    markers
        .iter()
        .filter_map(|marker| prompt.find(marker).map(|idx| (idx, *marker)))
        .min_by_key(|(idx, _)| *idx)
}

/// Walk `messages` in reverse and return the timestamp of the most
/// recent `role == "user"` message that carries a parseable
/// `timestamp` field. Messages serialize through serde of the typed
/// [`crabgent_core::Message`] enum, so the field is an RFC3339 string
/// when present.
fn last_user_timestamp(messages: &[serde_json::Value]) -> Option<DateTime<Utc>> {
    messages.iter().rev().find_map(|msg| {
        if msg.get("role")?.as_str()? != "user" {
            return None;
        }
        let raw = msg.get("timestamp")?.as_str()?;
        DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))
    })
}

#[cfg(test)]
mod strip_tests;

#[cfg(test)]
mod tests;
