//! Matrix policy configuration and builder.

pub mod builder;
pub mod channel_scope;
pub mod membership;
pub mod subject_resolver;
pub mod visibility;

pub use builder::build;
pub use channel_scope::{
    ChannelScopePolicy, build_with_channel_scope, build_with_channel_scope_multi,
};
pub use membership::MembershipIndex;
pub use subject_resolver::build_scoped_subject_resolver;
pub use visibility::{
    MapVisibilityResolver, MatrixVisibilityResolver, RoomVisibility, RoomVisibilityCache,
    VisibilityResolver, new_visibility_cache,
};

#[derive(Debug, Clone)]
pub struct MatrixPolicyConfig {
    pub allowed_users: Vec<String>,
    pub restricted_tools: Vec<String>,
}

impl Default for MatrixPolicyConfig {
    fn default() -> Self {
        Self {
            allowed_users: Vec::new(),
            restricted_tools: vec!["bash".into(), "write_file".into(), "update_file".into()],
        }
    }
}
