//! Participant identity and role types.
//!
//! `Participant` describes one entity inside a conversation. Roles are
//! `#[non_exhaustive]` plus a `Custom(String)` variant so external
//! crates can introduce new roles (`Customer`, `OnCall`, ...) without
//! patching this crate.
//!
//! `DirectRole` describes a 1:1 conversation's nature (`HumanAgent`,
//! `AgentAgent`). Initiator direction is not modelled: a
//! human-to-agent and an agent-to-human conversation share the same
//! `HumanAgent` role.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Opaque per-channel participant identifier (Slack user id, Telegram
/// user id, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParticipantId(String);

impl ParticipantId {
    /// Construct a `ParticipantId` from any string-like value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the inner string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ParticipantId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for ParticipantId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl AsRef<str> for ParticipantId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Role of a participant inside a conversation.
///
/// `Bot` is treated as synonymous with `Agent`: the distinction is
/// operational, not semantic, and would require multiple signals to draw
/// cleanly.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParticipantRole {
    /// A human user.
    Human,
    /// A bot or agent (LLM-driven or scripted automaton).
    Bot,
    /// An adapter-defined role (`Customer`, `OnCall`, ...).
    Custom(String),
}

impl ParticipantRole {
    /// Stable string identifier for this role, suitable for logging,
    /// matching, or `Subject::attrs`.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        match self {
            Self::Human => "human",
            Self::Bot => "bot",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl fmt::Display for ParticipantRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One participant inside a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    /// Channel-opaque identifier.
    pub id: ParticipantId,
    /// Role of this participant in the conversation.
    pub role: ParticipantRole,
    /// Optional human-readable label.
    pub display_name: Option<String>,
}

impl Participant {
    /// Build a new participant.
    pub fn new(id: impl Into<ParticipantId>, role: ParticipantRole) -> Self {
        Self {
            id: id.into(),
            role,
            display_name: None,
        }
    }

    /// Attach a display name, returning self for chaining.
    #[must_use]
    pub fn with_display_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }
}

/// The role of a `ChannelKind::Direct` conversation.
///
/// `HumanAgent` covers both human-to-agent and agent-to-human cases:
/// initiator direction is intentionally not modelled.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DirectRole {
    /// 1:1 conversation between a human and an agent.
    HumanAgent,
    /// 1:1 conversation between two agents.
    AgentAgent,
    /// Adapter-defined role.
    Custom(String),
}

impl DirectRole {
    /// Stable string identifier.
    #[must_use]
    pub const fn as_str(&self) -> &str {
        match self {
            Self::HumanAgent => "human_agent",
            Self::AgentAgent => "agent_agent",
            Self::Custom(s) => s.as_str(),
        }
    }
}

impl fmt::Display for DirectRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn participant_id_round_trips() {
        let a = ParticipantId::new("U123");
        let b = ParticipantId::from("U123".to_owned());
        let c: ParticipantId = "U123".into();
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn participant_id_display_matches_inner() {
        let p = ParticipantId::new("U-456");
        assert_eq!(format!("{p}"), "U-456");
        assert_eq!(p.as_str(), "U-456");
        assert_eq!(p.as_ref(), "U-456");
    }

    #[test]
    fn participant_id_serde_round_trip_is_transparent() {
        let p = ParticipantId::new("U999");
        let json = serde_json::to_string(&p).expect("serialize");
        assert_eq!(json, "\"U999\"");
        let back: ParticipantId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }

    #[test]
    fn participant_id_distinct_compare_unequal() {
        assert_ne!(ParticipantId::new("a"), ParticipantId::new("b"));
    }

    #[test]
    fn participant_role_known_variants_render() {
        assert_eq!(ParticipantRole::Human.as_str(), "human");
        assert_eq!(ParticipantRole::Bot.as_str(), "bot");
        assert_eq!(format!("{}", ParticipantRole::Human), "human");
    }

    #[test]
    fn participant_role_custom_uses_label() {
        let r = ParticipantRole::Custom("oncall".into());
        assert_eq!(r.as_str(), "oncall");
        assert_eq!(format!("{r}"), "oncall");
    }

    #[test]
    fn participant_builder_attaches_display_name() {
        let p = Participant::new("U1", ParticipantRole::Human).with_display_name("Alice");
        assert_eq!(p.id.as_str(), "U1");
        assert_eq!(p.role, ParticipantRole::Human);
        assert_eq!(p.display_name.as_deref(), Some("Alice"));
    }

    #[test]
    fn participant_default_display_name_is_none() {
        let p = Participant::new("B1", ParticipantRole::Bot);
        assert!(p.display_name.is_none());
    }

    #[test]
    fn participant_clone_is_independent() {
        let p1 = Participant::new("U1", ParticipantRole::Human).with_display_name("a");
        let p2 = p1.clone();
        assert_eq!(p1, p2);
    }

    #[test]
    fn direct_role_known_variants_render() {
        assert_eq!(DirectRole::HumanAgent.as_str(), "human_agent");
        assert_eq!(DirectRole::AgentAgent.as_str(), "agent_agent");
        assert_eq!(format!("{}", DirectRole::AgentAgent), "agent_agent");
    }

    #[test]
    fn direct_role_custom_uses_label() {
        let r = DirectRole::Custom("ops".into());
        assert_eq!(r.as_str(), "ops");
    }
}
