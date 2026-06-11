use std::str::FromStr;

use crabgent_channel_slack::SlackOwner;

#[test]
fn slack_owner_round_trips_with_slash_separator() {
    let owner = SlackOwner::from_str("slack:T123/C456").expect("owner should parse");

    assert_eq!(owner.workspace().as_str(), "T123");
    assert_eq!(owner.channel().as_str(), "C456");
    assert_eq!(owner.to_string(), "slack:T123/C456");
    assert_eq!(owner.owner().as_str(), "slack:T123/C456");
}
