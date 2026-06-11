//! `string_newtype!`: generate the shared impl block for opaque string newtypes.
//!
//! Several domain identifiers (`ModelId`, `SttModelId`, `TtsModelId`,
//! `VoiceId`, `ImageGenerationModelId`, `ImageGenerationSize`,
//! `ImageGenerationAspectRatio`, `Owner`, `ThreadId`) wrap a single `String`
//! and share an identical surface: an inherent `new` + `as_str`, a `Display`
//! that writes the inner string, and `From` conversions. The struct
//! declaration, its doc comment, and its derives stay at each definition site
//! so rustdoc and per-type derives remain explicit; only the repeated impl
//! block is generated here.
//!
//! Two construction modes:
//! - `trim`: `new` normalises to the trimmed form (model-id style).
//! - `passthrough`: `new` stores the value verbatim.
//!
//! Conversions follow the existing per-type surface:
//! - `trim` newtypes implement `From<String>`, `From<&str>`, `From<&String>`.
//! - `passthrough` newtypes implement `From<String>`, `From<&str>` and may add
//!   `AsRef<str>`.

/// Generate the inherent + trait impls shared by opaque string newtypes.
///
/// The wrapped type must already be declared as a tuple struct with a single
/// `String` field (`struct Name(String);`) together with its derives and doc
/// comment. See the module docs for the supported modes.
macro_rules! string_newtype {
    // Trim-on-construction newtypes: ModelId, SttModelId, TtsModelId, VoiceId,
    // ImageGenerationModelId. These carry the `From<&String>` conversion.
    (trim $name:ident) => {
        impl $name {
            #[doc = concat!("Construct a `", stringify!($name), "` from any owned-or-borrowed string.")]
            pub fn new(value: impl Into<String>) -> Self {
                let value = value.into();
                let trimmed = value.trim();
                if trimmed.len() == value.len() {
                    Self(value)
                } else {
                    Self(trimmed.to_owned())
                }
            }

            /// Borrow the underlying string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::convert::From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl ::std::convert::From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl ::std::convert::From<&String> for $name {
            fn from(value: &String) -> Self {
                Self::new(value.clone())
            }
        }
    };

    // Pass-through newtypes without `AsRef<str>`: ImageGenerationSize,
    // ImageGenerationAspectRatio. `From<String>` + `From<&str>` only.
    (passthrough $name:ident) => {
        impl $name {
            #[doc = concat!("Construct a `", stringify!($name), "` from any string-like value.")]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the inner string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::convert::From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl ::std::convert::From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }
    };

    // Pass-through newtypes that also expose `AsRef<str>`: Owner, ThreadId.
    (passthrough_as_ref $name:ident) => {
        impl $name {
            #[doc = concat!("Construct a `", stringify!($name), "` from any string-like value.")]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the inner string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl ::std::convert::From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl ::std::convert::From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl ::std::convert::AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

pub(crate) use string_newtype;
