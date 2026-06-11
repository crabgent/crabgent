use std::time::Duration;

use crabgent_channel_slack::SlackConfig;
use secrecy::SecretString;

#[test]
fn config_rejects_empty_tokens_and_invalid_runtime_values() {
    SlackConfig::new(
        SecretString::from(String::new()),
        SecretString::from("bot-test-token".to_owned()),
    )
    .expect_err("expected error");
    SlackConfig::new(
        SecretString::from("app-test-token".to_owned()),
        SecretString::from(String::new()),
    )
    .expect_err("expected error");

    let zero_timeout = SlackConfig::new(
        SecretString::from("app-test-token".to_owned()),
        SecretString::from("bot-test-token".to_owned()),
    )
    .expect("base config")
    .with_request_timeout(Duration::ZERO);
    assert!(zero_timeout.validate().is_err());

    let empty_api_base = SlackConfig::new(
        SecretString::from("app-test-token".to_owned()),
        SecretString::from("bot-test-token".to_owned()),
    )
    .expect("base config")
    .with_api_base(" ");
    assert!(empty_api_base.validate().is_err());

    let zero_body_cap = SlackConfig::new(
        SecretString::from("app-test-token".to_owned()),
        SecretString::from("bot-test-token".to_owned()),
    )
    .expect("base config")
    .with_body_cap_chars(0);
    assert!(zero_body_cap.validate().is_err());
}
