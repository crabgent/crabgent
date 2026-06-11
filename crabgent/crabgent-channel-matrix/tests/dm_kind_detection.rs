#[path = "support/mod.rs"]
mod support;

use crabgent_channel::{Channel, ChannelKind};
use crabgent_core::owner::Owner;

#[tokio::test]
async fn dm_and_group_kind_detection_use_room_membership() {
    let Some(dm) = support::dm_room()
        .await
        .expect("DM room fixture should initialize")
    else {
        return;
    };
    let dm_conv = Owner::new(format!("matrix:{}", dm.room_id));
    assert_eq!(
        dm.channel
            .kind(&dm_conv)
            .await
            .expect("DM channel kind should resolve"),
        ChannelKind::Direct
    );

    let Some(group) = support::joined_room(3)
        .await
        .expect("group room fixture should initialize")
    else {
        return;
    };
    assert!(group.bob.is_some());
    let group_conv = Owner::new(format!("matrix:{}", group.room_id));
    assert_eq!(
        group
            .channel
            .kind(&group_conv)
            .await
            .expect("group channel kind should resolve"),
        ChannelKind::Group
    );
}
