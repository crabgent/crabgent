//! Calendar lookups and a [`Hook`] that augments the LLM system prompt with
//! current time, week anchors, and holiday context.
//!
//! Two layers:
//!
//! - [`HolidayProvider`]: trait-shaped lookup over `(date, country, subdivision)`.
//! - [`EmbeddedHolidayProvider`]: default implementation backed by a multi-country
//!   JSON dataset embedded at compile time.
//!
//! Plug-in path: implement [`HolidayProvider`] for a custom data source (live
//! API, database, etc.) and pass it to [`TimeHintHook::new`].
//!
//! [`Hook`]: crabgent_core::Hook

mod annotate;
mod config;
mod hint_format;
mod hook;
mod provider;

pub use config::{Clock, TimeHintConfig};
pub use hint_format::PauseMarker;
pub use hook::{
    INLINE_ANNOTATE_LIMIT, TIME_GUIDANCE, TIME_HINT_CLOSE, TIME_HINT_CLOSE_MARKER,
    TIME_HINT_MARKER, TIME_HINT_OPEN, TimeHintHook,
};
pub use provider::{EmbeddedHolidayProvider, HolidayProvider};
