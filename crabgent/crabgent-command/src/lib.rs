//! Command dispatch for channel adapters.
//!
//! `CommandDispatchInbox` is an opt-in `ChannelInbox` decorator. It detects a
//! per-adapter text prefix, dispatches registered commands without running the
//! kernel, and records the user command plus assistant reply through
//! `SessionStore::save_messages`.

pub mod agent_name;
pub mod command;
pub mod error;
pub mod handles;
pub mod inbox;
pub mod name;
pub mod prefix;
pub mod registry;
pub mod tool_wrap;
pub mod wiring;

pub use agent_name::CommandAgentName;
pub use command::{Command, CommandCtx, CommandOutput};
pub use crabgent_store::SessionStore;
pub use error::{CommandAgentNameError, CommandError, CommandNameError, CommandPrefixError};
pub use handles::CommandHandles;
pub use inbox::CommandDispatchInbox;
pub use name::CommandName;
pub use prefix::CommandPrefix;
pub use registry::CommandRegistry;
pub use tool_wrap::ToolCommand;
pub use wiring::CommandWiring;
