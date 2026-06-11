//! Configuration for [`crate::MatrixChannel`].

use matrix_sdk::ruma::OwnedUserId;
use secrecy::SecretString;
use std::fmt::{self, Debug, Formatter};
use url::Url;

/// Default Matrix outbound body cap in bytes.
pub const DEFAULT_BODY_CAP_BYTES: usize = 65_536;

/// Matrix authentication input.
#[derive(Clone)]
pub enum MatrixAuth {
    /// Username / password login flow.
    Password { password: SecretString },

    /// Pre-authenticated token flow.
    AccessToken {
        access_token: SecretString,
        device_id: String,
    },
}

impl Debug for MatrixAuth {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password { .. } => f
                .debug_struct("Password")
                .field("password", &"<redacted>")
                .finish(),
            Self::AccessToken { device_id, .. } => f
                .debug_struct("AccessToken")
                .field("access_token", &"<redacted>")
                .field("device_id", device_id)
                .finish(),
        }
    }
}

/// Matrix runtime configuration used by [`crate::MatrixChannel`].
#[derive(Clone)]
pub struct MatrixChannelConfig {
    /// Homeserver base URL.
    pub homeserver: Url,

    /// MXID of the bot user.
    pub user: OwnedUserId,

    /// Authentication material.
    pub auth: MatrixAuth,

    /// Optional bot display name.
    pub bot_display_name: Option<String>,

    /// Maximum outbound message body length in UTF-8 bytes.
    pub body_cap_bytes: usize,
}

impl Debug for MatrixChannelConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let auth_name = match &self.auth {
            MatrixAuth::Password { .. } => "password",
            MatrixAuth::AccessToken { .. } => "access_token",
        };

        f.debug_struct("MatrixChannelConfig")
            .field("homeserver", &self.homeserver)
            .field("user", &self.user)
            .field("auth", &auth_name)
            .field("bot_display_name", &self.bot_display_name)
            .field("body_cap_bytes", &self.body_cap_bytes)
            .field("access_token", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::owned_user_id;

    #[test]
    fn debug_redacts_auth_material() {
        let config = MatrixChannelConfig {
            homeserver: Url::parse("https://example.org").expect("test result"),
            user: owned_user_id!("@bot:example.org"),
            auth: MatrixAuth::AccessToken {
                access_token: SecretString::from("secret-token".to_owned()),
                device_id: "DEVICE".into(),
            },
            bot_display_name: Some("Nova".into()),
            body_cap_bytes: DEFAULT_BODY_CAP_BYTES,
        };
        let rendered = format!("{config:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("secret-token"));
    }

    #[test]
    fn auth_debug_redacts_access_token_and_keeps_device_id() {
        let auth = MatrixAuth::AccessToken {
            access_token: SecretString::from("secret-token".to_owned()),
            device_id: "DEVICE".into(),
        };
        let rendered = format!("{auth:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("DEVICE"));
        assert!(!rendered.contains("secret-token"));
    }

    #[test]
    fn auth_debug_redacts_password() {
        let auth = MatrixAuth::Password {
            password: SecretString::from("secret-password".to_owned()),
        };
        let rendered = format!("{auth:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("secret-password"));
    }

    #[test]
    fn password_debug_redacts_password() {
        let config = MatrixChannelConfig {
            homeserver: Url::parse("https://example.org").expect("test result"),
            user: owned_user_id!("@bot:example.org"),
            auth: MatrixAuth::Password {
                password: SecretString::from("secret-password".to_owned()),
            },
            bot_display_name: None,
            body_cap_bytes: DEFAULT_BODY_CAP_BYTES,
        };
        let rendered = format!("{config:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("secret-password"));
    }
}
