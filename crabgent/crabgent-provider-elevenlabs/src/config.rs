//! `ElevenLabs` audio-provider configuration.

use std::fmt;

use secrecy::SecretString;

const DEFAULT_API_BASE: &str = "https://api.elevenlabs.io";

/// Runtime configuration injected by the host application.
#[derive(Clone)]
pub struct ElevenLabsConfig {
    pub api_key: SecretString,
    pub api_base: String,
}

impl ElevenLabsConfig {
    pub fn new(api_key: impl Into<SecretString>) -> Self {
        Self {
            api_key: api_key.into(),
            api_base: DEFAULT_API_BASE.to_owned(),
        }
    }

    #[must_use]
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into();
        self
    }

    #[must_use]
    pub fn api_base(&self) -> &str {
        &self.api_base
    }
}

impl fmt::Debug for ElevenLabsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ElevenLabsConfig")
            .field("api_key", &"****<masked>")
            .field("api_base", &self.api_base)
            .finish()
    }
}
