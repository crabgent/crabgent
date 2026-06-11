//! Shared inbound-test fixtures for `mod tests` and `mod tests_sanitize`.
//!
//! Both inbound test modules build a `matrix_sdk::Client` against a mock
//! homeserver and assemble an [`InboundMediaClients`] with most fields left
//! empty. These helpers fold those identical builders into one place; the
//! per-module event/timeline builders stay local because they diverge.

use crabgent_channel::AudioValidator;
use matrix_sdk::Client;
use url::Url;

use super::InboundMediaClients;

/// Build a `matrix_sdk::Client` pointed at `base_url` (a mock homeserver).
pub(super) async fn test_client_at(base_url: &str) -> Client {
    Client::new(Url::parse(base_url).expect("test result"))
        .await
        .expect("test result")
}

/// Build a `matrix_sdk::Client` pointed at the default placeholder homeserver.
pub(super) async fn test_client() -> Client {
    test_client_at("https://example.org").await
}

/// Assemble [`InboundMediaClients`] with no image store/validator and the
/// supplied audio validator plus access token.
pub(super) fn media_clients<'a>(
    matrix_client: &'a Client,
    image_http_client: &'a reqwest::Client,
    audio_http_client: &'a reqwest::Client,
    audio_validator: Option<&'a AudioValidator>,
    access_token: Option<&'a str>,
) -> InboundMediaClients<'a> {
    InboundMediaClients {
        matrix_client,
        image_http_client,
        image_store: None,
        image_validator: None,
        audio_http_client,
        audio_validator,
        access_token,
    }
}
