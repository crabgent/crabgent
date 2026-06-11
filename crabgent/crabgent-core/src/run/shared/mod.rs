//! Shared run-loop helpers used by sync and streaming drivers.

mod build_request;

#[cfg(test)]
mod tests;

pub(super) use build_request::*;
