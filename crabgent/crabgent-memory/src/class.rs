//! Memory class domain enum.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::MemoryError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryClass {
    Semantic,
    Episodic,
    Notes,
    UserProfile,
    Skill,
    Tools,
}

impl MemoryClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Episodic => "episodic",
            Self::Notes => "notes",
            Self::UserProfile => "user_profile",
            Self::Skill => "skill",
            Self::Tools => "tools",
        }
    }
}

impl fmt::Display for MemoryClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MemoryClass {
    type Err = MemoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "semantic" => Ok(Self::Semantic),
            "episodic" => Ok(Self::Episodic),
            "notes" => Ok(Self::Notes),
            "user_profile" => Ok(Self::UserProfile),
            "skill" => Ok(Self::Skill),
            "tools" => Ok(Self::Tools),
            other => Err(MemoryError::ParseClass(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_VARIANTS: [MemoryClass; 6] = [
        MemoryClass::Semantic,
        MemoryClass::Episodic,
        MemoryClass::Notes,
        MemoryClass::UserProfile,
        MemoryClass::Skill,
        MemoryClass::Tools,
    ];

    #[test]
    fn from_str_roundtrip() {
        for class in ALL_VARIANTS {
            assert_eq!(
                class.as_str().parse::<MemoryClass>().expect("parse class"),
                class
            );
            assert_eq!(class.to_string(), class.as_str());
        }
    }

    #[test]
    fn as_str_mapping() {
        assert_eq!(MemoryClass::Semantic.as_str(), "semantic");
        assert_eq!(MemoryClass::Episodic.as_str(), "episodic");
        assert_eq!(MemoryClass::Notes.as_str(), "notes");
        assert_eq!(MemoryClass::UserProfile.as_str(), "user_profile");
        assert_eq!(MemoryClass::Skill.as_str(), "skill");
        assert_eq!(MemoryClass::Tools.as_str(), "tools");
    }

    #[test]
    fn unknown_class_errors() {
        let err = "working"
            .parse::<MemoryClass>()
            .expect_err("expected error");
        assert!(matches!(err, MemoryError::ParseClass(raw) if raw == "working"));
    }

    #[test]
    fn serde_roundtrip_snake_case() {
        for class in ALL_VARIANTS {
            let json = serde_json::to_string(&class).expect("serialize");
            let expected = format!("\"{}\"", class.as_str());
            assert_eq!(json, expected, "serialize {class:?}");
            let back: MemoryClass = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, class, "deserialize {class:?}");
        }
    }

    #[test]
    fn display_renders_snake_case() {
        for class in ALL_VARIANTS {
            assert_eq!(class.to_string(), class.as_str());
        }
    }
}
