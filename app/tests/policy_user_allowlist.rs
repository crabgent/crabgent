use crabgent_channel::{attr_keys, channel_receive_action};
use crabgent_core::{
    Action,
    owner::Owner,
    policy::{PolicyDecision, PolicyHook},
    subject::Subject,
};
use crabgent_runtime::MatrixPolicyConfig;

fn receive_action() -> Action {
    channel_receive_action("matrix", &Owner::new("matrix:!room:example.org"))
}

#[tokio::test]
async fn allowlisted_matrix_user_can_receive() {
    let policy = crabgent_runtime::build(&MatrixPolicyConfig {
        allowed_users: vec!["@bob:example.org".into()],
        ..MatrixPolicyConfig::default()
    });
    let subject = Subject::new("matrix:%40bob%3Aexample.org")
        .with_attr(attr_keys::PARTICIPANT_ID, "@bob:example.org");

    assert!(matches!(
        policy.allow(&subject, &receive_action()).await,
        PolicyDecision::Allow
    ));
}

#[tokio::test]
async fn non_allowlisted_matrix_user_is_denied_receive() {
    let policy = crabgent_runtime::build(&MatrixPolicyConfig {
        allowed_users: vec!["@bob:example.org".into()],
        ..MatrixPolicyConfig::default()
    });
    let subject = Subject::new("matrix:%40mallory%3Aexample.org")
        .with_attr(attr_keys::PARTICIPANT_ID, "@mallory:example.org");

    assert!(matches!(
        policy.allow(&subject, &receive_action()).await,
        PolicyDecision::Deny(_)
    ));
}
