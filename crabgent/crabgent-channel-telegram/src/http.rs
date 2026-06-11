//! Hardened `reqwest` client construction for Telegram HTTP traffic.
//!
//! All outbound HTTP in this crate (Bot API calls and media downloads
//! built from `getFile` responses) goes through a client built here.
//! Bare `reqwest::Client::new()` follows redirects by default and has no
//! timeout: a redirecting or compromised CDN could 3xx an authenticated
//! request toward an internal host (blind SSRF), and a stalled response
//! is a slow-loris `DoS`. The bot token rides on these fetches, so a
//! same-origin redirect could forward credentials.

use std::time::Duration;

use crabgent_channel::ChannelError;

/// Connect-phase timeout shared by all Telegram HTTP clients.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Idle read timeout for the Bot API (long-poll) client. Bounds a
/// connection that stalls mid-response without capping the legitimate
/// long-poll wait, which is per-request server-side and may exceed any
/// fixed total timeout once a caller raises `with_poll_timeout`.
const BOT_API_READ_TIMEOUT: Duration = Duration::from_mins(2);

/// Total request timeout for media downloads. `getFile` plus the file
/// fetch is a finite request/response, so a total cap is correct here.
const MEDIA_TOTAL_TIMEOUT: Duration = Duration::from_mins(1);

/// Base builder with redirect following disabled.
///
/// `Policy::none` stops a redirect from an API or CDN host from silently
/// retargeting an authenticated request. Callers layer their timeout
/// strategy on top.
fn hardened_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
}

/// Build the Bot API client used for `getUpdates` long-polling and all
/// REST calls.
///
/// Uses an idle read timeout instead of a total timeout so a long-poll
/// wait (default 25 s, raisable via `with_poll_timeout`) is never aborted
/// mid-flight, while a stalled connection still trips the read timeout.
pub fn build_bot_api_client() -> Result<reqwest::Client, ChannelError> {
    hardened_builder()
        .read_timeout(BOT_API_READ_TIMEOUT)
        .build()
        .map_err(ChannelError::adapter)
}

/// Build a media-download client.
///
/// Media downloads are finite request/response, so a total timeout
/// bounds the whole exchange.
pub fn build_media_client() -> Result<reqwest::Client, ChannelError> {
    hardened_builder()
        .timeout(MEDIA_TOTAL_TIMEOUT)
        .build()
        .map_err(ChannelError::adapter)
}

#[cfg(test)]
mod tests {
    use httpmock::Method::GET;
    use httpmock::MockServer;

    use super::*;

    #[tokio::test]
    async fn media_client_does_not_follow_redirects() {
        // A 3xx from the file CDN must not be followed: an attacker-controlled
        // redirect could retarget the authenticated fetch at an internal host
        // (blind SSRF) and forward the bot token. The hardened client returns
        // the 3xx response verbatim instead.
        let server = MockServer::start();
        let redirect = server.mock(|when, then| {
            when.method(GET).path("/file/source");
            then.status(302)
                .header("location", "http://169.254.169.254/latest/meta-data/");
        });
        let target = server.mock(|when, then| {
            when.method(GET).path("/latest/meta-data/");
            then.status(200).body("internal-secret");
        });

        let client = build_media_client().expect("hardened media client builds");
        let response = client
            .get(server.url("/file/source"))
            .send()
            .await
            .expect("request reaches the mock server");

        assert_eq!(response.status().as_u16(), 302);
        redirect.assert();
        target.assert_calls(0);
        let body = response.text().await.expect("redirect body readable");
        assert!(!body.contains("internal-secret"));
    }

    #[test]
    fn bot_api_client_builds() {
        build_bot_api_client().expect("hardened bot api client builds");
    }
}
