//! Matrix bot adapter for crabgent channels.
//!
//! Supports group + direct rooms using Matrix sync-loop message streams.
//!
//! Operational note: token rotation via `/_matrix/client/v3/refresh` is not
//! implemented. When a token expires, the channel adapter surfaces a
//! `ChannelError`; operators must re-authenticate manually or configure a
//! scheduled re-login.

/// Keep this alias in sync with `tracing_test::traced_test` and `#[instrument]`
/// macro expectations, so tests keep using the canonical `tracing` path while
/// crate internals stay coupled to `crabgent_log`.
extern crate crabgent_log as tracing;

mod audio_download;
pub mod channel;
pub mod config;
pub mod error;
pub mod formatting;
pub mod image_download;
pub mod inbound;
pub mod outbound;
pub(crate) mod outbound_react;
pub(crate) mod reaction_tracker;
pub mod subject;
pub mod sync;
pub mod typing;

pub use channel::{MatrixChannel, RoomKindCache};
pub use config::{MatrixAuth, MatrixChannelConfig};
pub use error::MatrixChannelError;
pub use formatting::MATRIX_FORMATTING_HINT;
pub use subject::build_subject_resolver;
pub use sync::MatrixSyncPoller;
pub use typing::MatrixTypingIndicator;
