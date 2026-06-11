use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use matrix_sdk::{
    Client,
    ruma::{OwnedRoomId, RoomId, room::JoinRule},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomVisibility {
    Public,
    Private,
}

pub trait VisibilityResolver: Send + Sync {
    fn resolve(&self, room_id: &RoomId) -> RoomVisibility;
}

pub type RoomVisibilityCache = Arc<Mutex<HashMap<OwnedRoomId, RoomVisibility>>>;

#[must_use]
pub fn new_visibility_cache() -> RoomVisibilityCache {
    Arc::new(Mutex::new(HashMap::new()))
}

#[derive(Clone)]
pub struct MatrixVisibilityResolver {
    client: Client,
    cache: RoomVisibilityCache,
}

impl MatrixVisibilityResolver {
    #[must_use]
    pub const fn new(client: Client, cache: RoomVisibilityCache) -> Self {
        Self { client, cache }
    }

    fn cached(&self, room_id: &RoomId) -> Option<RoomVisibility> {
        self.cache.lock().ok()?.get(room_id).copied()
    }

    fn cache(&self, room_id: &RoomId, visibility: RoomVisibility) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(room_id.to_owned(), visibility);
        }
    }
}

impl VisibilityResolver for MatrixVisibilityResolver {
    fn resolve(&self, room_id: &RoomId) -> RoomVisibility {
        if let Some(visibility) = self.cached(room_id) {
            return visibility;
        }

        let Some(room) = self.client.get_room(room_id) else {
            return RoomVisibility::Private;
        };
        let Some(join_rule) = room.join_rule() else {
            return RoomVisibility::Private;
        };
        let visibility = if matches!(join_rule, JoinRule::Public) {
            RoomVisibility::Public
        } else {
            RoomVisibility::Private
        };
        self.cache(room_id, visibility);
        visibility
    }
}

#[derive(Debug, Clone, Default)]
pub struct MapVisibilityResolver {
    map: HashMap<OwnedRoomId, RoomVisibility>,
}

impl MapVisibilityResolver {
    #[must_use]
    pub const fn new(map: HashMap<OwnedRoomId, RoomVisibility>) -> Self {
        Self { map }
    }
}

impl VisibilityResolver for MapVisibilityResolver {
    fn resolve(&self, room_id: &RoomId) -> RoomVisibility {
        self.map
            .get(room_id)
            .copied()
            .unwrap_or(RoomVisibility::Private)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc};

    use matrix_sdk::{Client, reqwest::Url, ruma::OwnedRoomId};

    use super::{
        MapVisibilityResolver, MatrixVisibilityResolver, RoomVisibility, VisibilityResolver,
        new_visibility_cache,
    };

    fn room_id(raw: &str) -> OwnedRoomId {
        OwnedRoomId::try_from(raw.to_owned()).expect("valid room id")
    }

    #[test]
    fn map_resolver_returns_public_when_present() {
        let public_room = room_id("!public:example.org");
        let resolver = MapVisibilityResolver::new(HashMap::from([(
            public_room.clone(),
            RoomVisibility::Public,
        )]));

        assert_eq!(resolver.resolve(&public_room), RoomVisibility::Public);
    }

    #[test]
    fn map_resolver_returns_private_when_missing() {
        let resolver = MapVisibilityResolver::default();
        let room = room_id("!missing:example.org");

        assert_eq!(resolver.resolve(&room), RoomVisibility::Private);
    }

    #[test]
    fn visibility_enum_derives_eq() {
        assert_eq!(RoomVisibility::Public, RoomVisibility::Public);
        assert_ne!(RoomVisibility::Public, RoomVisibility::Private);
    }

    #[tokio::test]
    async fn matrix_resolver_compiles() {
        let client = Client::new(Url::parse("https://example.org").expect("valid url"))
            .await
            .expect("client builds");
        let resolver = MatrixVisibilityResolver::new(client, new_visibility_cache());
        let room = room_id("!unknown:example.org");

        assert_eq!(resolver.resolve(&room), RoomVisibility::Private);
    }

    #[tokio::test]
    async fn matrix_resolver_does_not_cache_when_room_is_unknown() {
        // `matrix_sdk::Room` is not constructible here without pulling in
        // `matrix_sdk_test::JoinedRoomBuilder`, so this guards the same
        // fail-closed no-cache contract through the unknown-room path.
        let client = Client::new(Url::parse("https://example.org").expect("valid url"))
            .await
            .expect("client builds");
        let cache = new_visibility_cache();
        let resolver = MatrixVisibilityResolver::new(client, Arc::clone(&cache));
        let room = room_id("!unknown:example.org");

        assert_eq!(resolver.resolve(&room), RoomVisibility::Private);
        assert!(cache.lock().expect("visibility cache lock").is_empty());
    }
}
