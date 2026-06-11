//! Library surface for runtime integration helpers.

mod policy;

pub use policy::{
    ChannelScopePolicy, MapVisibilityResolver, MatrixPolicyConfig, MatrixVisibilityResolver,
    MembershipIndex, RoomVisibility, RoomVisibilityCache, VisibilityResolver, build,
    build_scoped_subject_resolver, build_with_channel_scope, build_with_channel_scope_multi,
    new_visibility_cache,
};
