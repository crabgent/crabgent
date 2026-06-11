//! Auto-accept Matrix room invites from allowlisted users.
//!
//! Registers a matrix-sdk event handler that watches for
//! `m.room.member` events targeting the bot with `membership = invite`.
//! If the inviter is in the agent's allowlist, the bot joins the room.
//! All other invites are left pending so they can be inspected manually.

use std::collections::HashSet;
use std::sync::Arc;

use crabgent_log::{info, warn};
use matrix_sdk::{
    Client, Room,
    ruma::{
        OwnedUserId,
        events::room::member::{MembershipState, StrippedRoomMemberEvent},
    },
};

pub fn register_auto_accept(client: &Client, bot_user_id: OwnedUserId, allowed_users: &[String]) {
    if allowed_users.is_empty() {
        return;
    }
    let allowed: Arc<HashSet<OwnedUserId>> = Arc::new(
        allowed_users
            .iter()
            .filter_map(|raw| match OwnedUserId::try_from(raw.as_str()) {
                Ok(id) => Some(id),
                Err(err) => {
                    warn!(user = %raw, error = %err, "skip invalid mxid in allowed_users");
                    None
                }
            })
            .collect(),
    );
    let bot_user_id = Arc::new(bot_user_id);
    client.add_event_handler(move |ev: StrippedRoomMemberEvent, room: Room| {
        let allowed = Arc::clone(&allowed);
        let bot_user_id = Arc::clone(&bot_user_id);
        async move {
            if ev.content.membership != MembershipState::Invite {
                return;
            }
            if ev.state_key.as_str() != bot_user_id.as_str() {
                return;
            }
            let inviter = ev.sender.clone();
            if !allowed.contains(&inviter) {
                warn!(
                    inviter = %inviter,
                    room = %room.room_id(),
                    "ignoring invite from non-allowlisted user"
                );
                return;
            }
            match room.join().await {
                Ok(()) => info!(
                    inviter = %inviter,
                    room = %room.room_id(),
                    "auto-joined room on allowlisted invite"
                ),
                Err(err) => warn!(
                    inviter = %inviter,
                    room = %room.room_id(),
                    error = %err,
                    "auto-join failed"
                ),
            }
        }
    });
}
