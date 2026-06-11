use crabgent_channel_slack::SlackConfig;
use secrecy::SecretString;

#[test]
fn config_debug_redacts_tokens() {
    let config = SlackConfig::new(
        SecretString::from("secret-app-token".to_owned()),
        SecretString::from("secret-bot-token".to_owned()),
    )
    .expect("valid config");

    let debug = format!("{config:?}");

    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("secret-app-token"));
    assert!(!debug.contains("secret-bot-token"));
}
