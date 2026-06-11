use std::str::FromStr;

use crabgent_channel_slack::{
    SlackChannelId, SlackOwner, SlackTs, SlackUserGroupId, SlackWorkspaceId,
};

#[test]
fn ids_reject_invalid_shapes() {
    SlackWorkspaceId::new("team").expect_err("expected error");
    SlackChannelId::new("C space").expect_err("expected error");
    SlackUserGroupId::new("U123").expect_err("expected error");
    SlackTs::new("not-a-ts").expect_err("expected error");
    SlackOwner::from_str("telegram:T1/C1").expect_err("expected error");
    SlackOwner::from_str("slack:T1").expect_err("expected error");
}
