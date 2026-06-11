//! HTTP webhook trait surface for channel adapters.
//!
//! This module defines [`WebhookHandler`] for receiving an HTTP webhook
//! body and turning it into adapter-side action, plus [`SignatureVerify`]
//! for validating request signatures such as Slack
//! `X-Slack-Signature` or Telegram secret-token headers.
//!
//! There is no HTTP server here. Channel-adapter crates pick their own
//! server runtime and forward inbound requests as a [`WebhookRequest`].

mod handler;
mod signature;

pub use handler::{WebhookHandler, WebhookRequest, WebhookResponse};
pub use signature::{NoopSignatureVerify, SignatureVerify};
