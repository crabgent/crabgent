//! `CommandDispatchInbox` tests, grouped by surface.
//!
//! Shared fixtures live in [`helpers`]; the test functions split into
//! dispatch/routing, policy/subject, and persistence/reply groups so each
//! file stays under the 500-line cap.

mod helpers;

mod dispatch;
mod persistence;
mod policy;
