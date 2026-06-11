//! Retry helpers for Slack Web API calls.

use std::future::Future;
use std::time::Duration;

use tokio::time::sleep;

use crate::error::SlackError;

pub(super) async fn rate_limited<T, F, Fut>(retry_max: u32, mut send: F) -> Result<T, SlackError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, SlackError>>,
{
    let mut attempt = 0;
    loop {
        match send().await {
            Err(SlackError::RateLimited { retry_after }) if attempt < retry_max => {
                attempt += 1;
                sleep(retry_after.unwrap_or_else(|| backoff(attempt))).await;
            }
            Err(error) => return Err(error),
            Ok(value) => return Ok(value),
        }
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(100 * u64::from(attempt))
}
