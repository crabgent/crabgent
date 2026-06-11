use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, RwLock},
    time::Duration,
};

use crabgent_log::warn;
use matrix_sdk::{
    Client, Error,
    ruma::{OwnedRoomId, OwnedUserId, UserId},
};
use tokio::time::sleep;

#[derive(Debug)]
pub struct MembershipIndex {
    inner: Arc<RwLock<HashMap<OwnedUserId, BTreeSet<OwnedRoomId>>>>,
    agent_user_id: OwnedUserId,
}

impl MembershipIndex {
    #[must_use]
    pub fn new(agent_user_id: OwnedUserId) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            agent_user_id,
        }
    }

    pub async fn refresh(&self, client: &Client) -> Result<(), Error> {
        let mut next = HashMap::<OwnedUserId, BTreeSet<OwnedRoomId>>::new();
        for room in client.joined_rooms() {
            let members = room.joined_user_ids().await.map_err(Error::from)?;
            for member in members {
                next.entry(member)
                    .or_default()
                    .insert(room.room_id().to_owned());
            }
        }
        self.replace_inner(next);
        Ok(())
    }

    #[must_use]
    pub fn shared_with(&self, user_id: &UserId) -> Vec<OwnedRoomId> {
        let Ok(inner) = self.inner.read() else {
            return Vec::new();
        };
        let Some(user_rooms) = inner.get(user_id) else {
            return Vec::new();
        };
        let Some(agent_rooms) = inner.get(&self.agent_user_id) else {
            return Vec::new();
        };
        user_rooms.intersection(agent_rooms).cloned().collect()
    }

    pub async fn run_refresher_loop(index: Arc<Self>, client: Client, interval: Duration) {
        loop {
            if let Err(err) = index.refresh(&client).await {
                warn!("matrix membership refresh failed: {err}");
            }
            sleep(interval).await;
        }
    }

    fn replace_inner(&self, next: HashMap<OwnedUserId, BTreeSet<OwnedRoomId>>) {
        if let Ok(mut inner) = self.inner.write() {
            *inner = next;
        } else {
            warn!("matrix membership index lock poisoned during refresh");
        }
    }

    #[cfg(test)]
    pub(crate) fn insert_for_test(&self, user_id: OwnedUserId, room_id: OwnedRoomId) {
        self.inner
            .write()
            .expect("membership test lock")
            .entry(user_id)
            .or_default()
            .insert(room_id);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};

    use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};

    use super::MembershipIndex;

    fn user_id(raw: &str) -> OwnedUserId {
        OwnedUserId::try_from(raw.to_owned()).expect("valid user id")
    }

    fn room_id(raw: &str) -> OwnedRoomId {
        OwnedRoomId::try_from(raw.to_owned()).expect("valid room id")
    }

    #[test]
    fn new_index_is_empty() {
        let index = MembershipIndex::new(user_id("@agent:example.org"));

        assert!(index.shared_with(&user_id("@alice:example.org")).is_empty());
    }

    #[test]
    fn shared_with_returns_empty_for_unknown_user() {
        let index = MembershipIndex::new(user_id("@agent:example.org"));
        index.insert_for_test(user_id("@agent:example.org"), room_id("!room:example.org"));

        assert!(
            index
                .shared_with(&user_id("@unknown:example.org"))
                .is_empty()
        );
    }

    #[test]
    fn shared_with_returns_sorted_rooms_after_test_insert() {
        let index = MembershipIndex::new(user_id("@agent:example.org"));
        let agent = user_id("@agent:example.org");
        let alice = user_id("@alice:example.org");
        let room_a = room_id("!a:example.org");
        let room_b = room_id("!b:example.org");
        index.insert_for_test(agent.clone(), room_b.clone());
        index.insert_for_test(agent, room_a.clone());
        index.insert_for_test(alice.clone(), room_b.clone());
        index.insert_for_test(alice.clone(), room_a.clone());

        assert_eq!(index.shared_with(&alice), vec![room_a, room_b]);
    }

    #[tokio::test]
    async fn refresh_replaces_atomically_with_stub() {
        let index = MembershipIndex::new(user_id("@agent:example.org"));
        let agent = user_id("@agent:example.org");
        let alice = user_id("@alice:example.org");
        let old_room = room_id("!old:example.org");
        let new_room = room_id("!new:example.org");
        index.insert_for_test(agent.clone(), old_room.clone());
        index.insert_for_test(alice.clone(), old_room);

        index.replace_inner(HashMap::from([
            (agent, BTreeSet::from([new_room.clone()])),
            (alice.clone(), BTreeSet::from([new_room.clone()])),
        ]));

        assert_eq!(index.shared_with(&alice), vec![new_room]);
    }
}
