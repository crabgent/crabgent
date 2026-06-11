//! `ModelId`: typed identifier for an LLM model.
//!
//! Built as a newtype around `String` so the kernel can validate
//! incoming model selections against a registry without conflating
//! arbitrary strings with registered identifiers.

use serde::{Deserialize, Serialize};

use crate::newtype::string_newtype;

/// Stable identifier of a model registered with a [`Provider`].
///
/// The contained string is normalised to its trimmed form on
/// construction. Aliases for a single model live as additional
/// `ModelId`s on the corresponding `ModelInfo`.
///
/// Serializes/deserializes as a transparent string so existing wire
/// formats (`{"model": "claude-haiku-4-5"}`) keep working.
///
/// [`Provider`]: crate::provider::Provider
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

string_newtype!(trim ModelId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_trims_whitespace() {
        let id = ModelId::new("  claude-haiku-4-5  ");
        assert_eq!(id.as_str(), "claude-haiku-4-5");
    }

    #[test]
    fn from_str_works() {
        let id: ModelId = "claude-sonnet-4-6".into();
        assert_eq!(id.as_str(), "claude-sonnet-4-6");
    }

    #[test]
    fn from_string_works() {
        let id: ModelId = String::from("claude-opus-4-7").into();
        assert_eq!(id.as_str(), "claude-opus-4-7");
    }

    #[test]
    fn from_string_ref_works() {
        let s = String::from("haiku");
        let id: ModelId = (&s).into();
        assert_eq!(id.as_str(), "haiku");
    }

    #[test]
    fn equal_ids_hash_equal() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let a = ModelId::new("x");
        let b = ModelId::new(" x ");
        let mut ha = DefaultHasher::new();
        let mut hb = DefaultHasher::new();
        a.hash(&mut ha);
        b.hash(&mut hb);
        assert_eq!(ha.finish(), hb.finish());
        assert_eq!(a, b);
    }

    #[test]
    fn display_renders_inner() {
        let id = ModelId::new("m");
        assert_eq!(format!("{id}"), "m");
    }
}
