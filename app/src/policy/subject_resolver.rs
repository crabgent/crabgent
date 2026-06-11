use std::sync::Arc;

use crabgent_channel::{InboundEvent, attr_keys};
use crabgent_channel_matrix::{MatrixChannel, outbound::parse_owner_to_room_id};
use crabgent_core::subject::Subject;
use matrix_sdk::ruma::{RoomId, UserId};

use super::{
    membership::MembershipIndex,
    visibility::{RoomVisibility, VisibilityResolver},
};

pub fn build_scoped_subject_resolver(
    channel: Arc<MatrixChannel>,
    agent_name: String,
    visibility: Arc<dyn VisibilityResolver + Send + Sync>,
    membership: Arc<MembershipIndex>,
) -> impl Fn(&InboundEvent) -> Subject + Send + Sync + 'static {
    let inner = crabgent_channel_matrix::build_subject_resolver(channel, agent_name);
    move |event| {
        let mut subject = inner(event);
        subject = with_visibility(subject, event, visibility.as_ref());
        if subject.attr(attr_keys::CHANNEL_KIND) == Some("direct") {
            subject = with_shared_room_ids(subject, event, &membership);
        }
        subject
    }
}

fn with_visibility(
    subject: Subject,
    event: &InboundEvent,
    visibility: &(dyn VisibilityResolver + Send + Sync),
) -> Subject {
    let Ok(room_id) = parse_owner_to_room_id(&event.conv) else {
        return subject;
    };
    let value = match visibility.resolve(&room_id) {
        RoomVisibility::Public => "public",
        RoomVisibility::Private => "private",
    };
    subject.with_attr("channel_visibility", value)
}

fn with_shared_room_ids(
    subject: Subject,
    event: &InboundEvent,
    membership: &MembershipIndex,
) -> Subject {
    let Ok(user_id) = UserId::parse(event.from.id.as_str()) else {
        return subject.with_attr("shared_room_ids", "");
    };
    let joined = membership
        .shared_with(&user_id)
        .iter()
        .map(|room_id| owner_for_room(room_id.as_ref()))
        .collect::<Vec<_>>()
        .join(",");
    subject.with_attr("shared_room_ids", joined)
}

fn owner_for_room(room_id: &RoomId) -> String {
    format!("matrix:{room_id}")
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use crabgent_channel::{ChannelKind, InboundEvent, MessageRef, Participant, ParticipantRole};
    use crabgent_channel_matrix::MatrixChannel;
    use crabgent_core::owner::Owner;
    use matrix_sdk::{
        Client,
        reqwest::Url,
        ruma::{OwnedRoomId, OwnedUserId},
    };

    use super::build_scoped_subject_resolver;
    use crate::policy::{
        MapVisibilityResolver, MembershipIndex, RoomVisibility, VisibilityResolver,
    };

    fn room_id(raw: &str) -> OwnedRoomId {
        OwnedRoomId::try_from(raw.to_owned()).expect("valid room id")
    }

    fn user_id(raw: &str) -> OwnedUserId {
        OwnedUserId::try_from(raw.to_owned()).expect("valid user id")
    }

    async fn channel_with_kind(room_id: &OwnedRoomId, kind: ChannelKind) -> Arc<MatrixChannel> {
        let client = Client::new(Url::parse("https://example.org").expect("valid url"))
            .await
            .expect("client builds");
        let channel = Arc::new(MatrixChannel::from_client(
            client,
            user_id("@agent:example.org"),
            None,
        ));
        channel
            .kind_cache()
            .lock()
            .expect("kind cache lock")
            .insert(room_id.clone(), kind);
        channel
    }

    fn event(room_id: &OwnedRoomId, from: &str) -> InboundEvent {
        let conv = Owner::new(format!("matrix:{room_id}"));
        InboundEvent {
            channel: "matrix".to_owned(),
            conv: conv.clone(),
            kind: None,
            from: Participant::new(from, ParticipantRole::Human),
            message: MessageRef::top_level("matrix", conv, "$event:example.org"),
            body: "hello".to_owned(),
            timestamp: "1970-01-01T00:00:00Z".parse().expect("valid timestamp"),
            attachments: vec![],
        }
    }

    fn visibility_resolver(
        room_id: OwnedRoomId,
        visibility: RoomVisibility,
    ) -> Arc<dyn VisibilityResolver + Send + Sync> {
        Arc::new(MapVisibilityResolver::new(HashMap::from([(
            room_id, visibility,
        )])))
    }

    #[tokio::test]
    async fn visibility_attr_set_for_public_room() {
        let room = room_id("!public:example.org");
        let channel = channel_with_kind(&room, ChannelKind::Group).await;
        let membership = Arc::new(MembershipIndex::new(user_id("@agent:example.org")));
        let resolver = build_scoped_subject_resolver(
            channel,
            "agent".to_owned(),
            visibility_resolver(room.clone(), RoomVisibility::Public),
            membership,
        );

        let subject = resolver(&event(&room, "@alice:example.org"));

        assert_eq!(subject.attr("channel_visibility"), Some("public"));
    }

    #[tokio::test]
    async fn visibility_attr_set_for_private_room() {
        let room = room_id("!private:example.org");
        let channel = channel_with_kind(&room, ChannelKind::Group).await;
        let membership = Arc::new(MembershipIndex::new(user_id("@agent:example.org")));
        let resolver = build_scoped_subject_resolver(
            channel,
            "agent".to_owned(),
            visibility_resolver(room.clone(), RoomVisibility::Private),
            membership,
        );

        let subject = resolver(&event(&room, "@alice:example.org"));

        assert_eq!(subject.attr("channel_visibility"), Some("private"));
    }

    #[tokio::test]
    async fn shared_room_ids_set_for_direct_subject_with_overlap() {
        let dm = room_id("!dm:example.org");
        let shared = room_id("!shared:example.org");
        let agent = user_id("@agent:example.org");
        let alice = user_id("@alice:example.org");
        let channel = channel_with_kind(&dm, ChannelKind::Direct).await;
        let membership = Arc::new(MembershipIndex::new(agent.clone()));
        membership.insert_for_test(agent, shared.clone());
        membership.insert_for_test(alice.clone(), shared.clone());
        let resolver = build_scoped_subject_resolver(
            channel,
            "agent".to_owned(),
            visibility_resolver(dm.clone(), RoomVisibility::Private),
            membership,
        );

        let subject = resolver(&event(&dm, alice.as_str()));

        assert_eq!(
            subject.attr("shared_room_ids"),
            Some(format!("matrix:{shared}").as_str())
        );
    }

    #[tokio::test]
    async fn shared_room_ids_empty_string_when_no_overlap() {
        let dm = room_id("!dm:example.org");
        let channel = channel_with_kind(&dm, ChannelKind::Direct).await;
        let membership = Arc::new(MembershipIndex::new(user_id("@agent:example.org")));
        let resolver = build_scoped_subject_resolver(
            channel,
            "agent".to_owned(),
            visibility_resolver(dm.clone(), RoomVisibility::Private),
            membership,
        );

        let subject = resolver(&event(&dm, "@alice:example.org"));

        assert_eq!(subject.attr("shared_room_ids"), Some(""));
    }
}
