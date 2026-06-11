//! Slack agent-progress surface.
//!
//! Holds the channel-specific progress trait (`SlackAgentProgress`) plus
//! its indicator/hook bridge wiring. `types` is the always-present
//! contract: the trait, the `ProgressChunk` enum, the `SlackAppType`
//! auto-detection enum, the heartbeat cadence constant, and the
//! sentinel-error code list. `indicator` and `hook` are populated by
//! later bullets in the same plan.

pub mod consumer;
pub mod hook;
pub mod indicator;
pub mod types;

pub use hook::{DEFAULT_SILENT_TOOLS, SlackAgentProgressHook};
pub use indicator::SlackAgentProgressIndicator;
pub use types::{
    AGENT_STATUS_HEARTBEAT_INTERVAL, AgentProgressConfig, AgentProgressError, AgentProgressResult,
    DEFAULT_IDLE_FLUSH_INTERVAL, NoopSlackAgentProgress, ProgressChunk, SENTINEL_NOT_AGENT_ERRORS,
    SlackAgentProgress, SlackAppType,
};
