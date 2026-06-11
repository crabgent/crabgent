mod common;

use crabgent_channel_slack::tools::register_slack_tools;
use std::collections::HashSet;

use common::{allow_policy, slack_client, slack_test_ctx};

#[tokio::test]
async fn register_slack_tools_exposes_expected_tool_set() {
    let ctx = slack_test_ctx().await;
    let tools = register_slack_tools(slack_client(&ctx), allow_policy());

    let actual: Vec<_> = tools.iter().map(|tool| tool.name()).collect();
    let expected = ["slack_search"];
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 1);

    let uniq: HashSet<_> = actual.iter().collect();
    assert_eq!(uniq.len(), 1);
    assert!(tools.iter().all(|tool| !tool.description().is_empty()));
}
