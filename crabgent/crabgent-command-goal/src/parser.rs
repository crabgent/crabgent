//! `/goal` sub-command parsing.

/// A parsed `/goal` sub-command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalCmd {
    /// `/goal` with no argument: show the current goal.
    Show,
    /// `/goal <objective>`: set (create or replace) the thread objective.
    Set(String),
    /// `/goal pause`.
    Pause,
    /// `/goal resume`.
    Resume,
    /// `/goal clear`.
    Clear,
}

impl GoalCmd {
    /// Parse the raw command input (everything after `/goal`).
    ///
    /// The bare keywords `pause`, `resume`, and `clear` are control verbs; any
    /// other non-empty input is treated as an objective to set. Empty input
    /// shows the current goal.
    #[must_use]
    pub fn parse(input: &str) -> Self {
        let trimmed = input.trim();
        match trimmed {
            "" => Self::Show,
            "pause" => Self::Pause,
            "resume" => Self::Resume,
            "clear" => Self::Clear,
            objective => Self::Set(objective.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_shows() {
        assert_eq!(GoalCmd::parse(""), GoalCmd::Show);
        assert_eq!(GoalCmd::parse("   "), GoalCmd::Show);
    }

    #[test]
    fn control_verbs_parse() {
        assert_eq!(GoalCmd::parse("pause"), GoalCmd::Pause);
        assert_eq!(GoalCmd::parse("resume"), GoalCmd::Resume);
        assert_eq!(GoalCmd::parse("clear"), GoalCmd::Clear);
    }

    #[test]
    fn other_input_is_an_objective() {
        assert_eq!(
            GoalCmd::parse("  ship the release  "),
            GoalCmd::Set("ship the release".to_owned())
        );
        // A verb embedded in a longer objective is not a control verb.
        assert_eq!(
            GoalCmd::parse("pause the rollout"),
            GoalCmd::Set("pause the rollout".to_owned())
        );
    }
}
