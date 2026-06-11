//! Identity pairing for push-style channels.
//!
//! Push-style channel adapters (Telegram, Matrix, Signal, ...) often
//! receive inbound messages from arbitrary user IDs without an
//! upfront authentication step. The kernel needs an explicit pairing
//! handshake so an unknown sender cannot drive a kernel run as if it
//! were an authorised user.
//!
//! This module provides:
//!
//! - [`PairingStore`]: trait + two impls (`MemoryPairingStore` for
//!   tests, `FilePairingStore` for production) that persist a set of
//!   paired user-id strings.
//! - [`PairingInbox`]: a [`crate::inbox::ChannelInbox`] decorator
//!   that intercepts `/pair <token>` commands, gates kernel
//!   dispatch behind the paired-set, and replies via a
//!   [`crate::sink::ChannelSink`].
//!
//! Usage pattern:
//! ```text
//! KernelChannelInbox -> wrap with PairingInbox -> register with adapter
//! ```
//!
//! The token is supplied as a static `String` at construction. Bring
//! your own dynamic resolver (e.g. credential broker) by mutating
//! `pair_token` via interior mutability before `receive()` if
//! needed; the trait surface stays simple.

mod inbox;
mod store;

pub use inbox::PairingInbox;
pub use store::{FilePairingStore, MemoryPairingStore, PairingStore};
