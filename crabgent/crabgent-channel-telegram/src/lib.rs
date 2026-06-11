//! Telegram bot adapter for crabgent channels.
//!
//! Direct-only reference implementation: 1:1 conversations between
//! a Telegram user and the bot. Group/Supergroup/Channel
//! conversations are out-of-scope for this pass (Telegram-API has
//! no list-all-members endpoint).
//!
//! Transport is pure REST + Long-Polling (`getUpdates`). No
//! webhook server, no WebSocket. The poller drives a
//! [`crabgent_channel::ChannelInbox`] in a background task.
//!
//! Pairing handshake (`/pair <token>`) is supplied by
//! [`crabgent_channel::pairing::PairingInbox`]; this crate does not
//! re-implement it.

/// Keep this alias in sync with `tracing_test::traced_test` and `#[instrument]`
/// macro expectations, so tests keep using the canonical `tracing` path while
/// crate internals stay coupled to `crabgent_log`.
extern crate crabgent_log as tracing;

mod audio_download;
pub mod channel;
pub mod formatting;
mod http;
pub mod image_download;
mod outbound;
pub mod photo_types;
pub mod poller;
mod react;
pub mod typing;

pub use channel::TelegramChannel;
pub use formatting::TELEGRAM_FORMATTING_HINT;
pub use poller::TelegramPoller;
pub use typing::TelegramTypingIndicator;
