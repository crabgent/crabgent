use crabgent_channel_slack::{SlackUserId, SlackWorkspaceId, slack_subject_id};

#[test]
fn slack_subject_id_is_deterministic() {
    let workspace = SlackWorkspaceId::new("T123").expect("workspace");
    let user = SlackUserId::new("U456").expect("user");

    assert_eq!(slack_subject_id(&workspace, &user), "slack:T123/U456");
    assert_eq!(
        slack_subject_id(&workspace, &user),
        slack_subject_id(&workspace, &user)
    );
}
