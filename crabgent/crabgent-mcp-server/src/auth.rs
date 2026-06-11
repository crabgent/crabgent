use std::collections::BTreeMap;

use secrecy::{ExposeSecret, SecretString};
use subtle::ConstantTimeEq;

use crate::McpServerError;

pub const AUTHORIZATION_HEADER: &str = "authorization";
const BEARER_PREFIX: &str = "Bearer ";

pub type HeaderMap = BTreeMap<String, String>;

pub fn verify_bearer(
    headers: &HeaderMap,
    bearer_token: &SecretString,
) -> Result<(), McpServerError> {
    verified_bearer(headers, bearer_token).map(|_bearer| ())
}

pub fn verified_bearer<'a>(
    headers: &'a HeaderMap,
    bearer_token: &SecretString,
) -> Result<&'a str, McpServerError> {
    let header = authorization_header(headers).ok_or(McpServerError::AuthRequired)?;
    let candidate = header
        .strip_prefix(BEARER_PREFIX)
        .ok_or(McpServerError::AuthRequired)?;

    if candidate.is_empty() {
        return Err(McpServerError::AuthRequired);
    }

    let expected = bearer_token.expose_secret().as_bytes();
    let provided = candidate.as_bytes();
    bool::from(provided.ct_eq(expected))
        .then_some(candidate)
        .ok_or(McpServerError::AuthRequired)
}

fn authorization_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .iter()
        .find(|(name, _value)| name.eq_ignore_ascii_case(AUTHORIZATION_HEADER))
        .map(|(_name, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_TOKEN: &str = "secret-test-token-12345";

    fn token() -> SecretString {
        SecretString::from(TEST_TOKEN)
    }

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION_HEADER.to_owned(), value.to_owned());
        headers
    }

    fn assert_no_token_leak(error: &McpServerError) {
        assert!(!error.to_string().contains(TEST_TOKEN));
        assert!(!format!("{error:?}").contains(TEST_TOKEN));
    }

    #[test]
    fn verify_bearer_happy() {
        let headers = headers("Bearer secret-test-token-12345");

        verify_bearer(&headers, &token()).expect("matching bearer token is valid");
    }

    #[test]
    fn verify_bearer_missing_returns_auth_required_no_leak() {
        let headers = HeaderMap::new();
        let error = verify_bearer(&headers, &token()).expect_err("missing bearer must fail");

        assert!(matches!(error, McpServerError::AuthRequired));
        assert_no_token_leak(&error);
    }

    #[test]
    fn verify_bearer_wrong_no_leak() {
        let headers = headers("Bearer wrong-token");
        let error = verify_bearer(&headers, &token()).expect_err("wrong bearer must fail");

        assert!(matches!(error, McpServerError::AuthRequired));
        assert_no_token_leak(&error);
    }

    #[test]
    fn verify_bearer_malformed_no_leak() {
        let headers = headers("Basic secret-test-token-12345");
        let error = verify_bearer(&headers, &token()).expect_err("malformed auth must fail");

        assert!(matches!(error, McpServerError::AuthRequired));
        assert_no_token_leak(&error);
    }
}
