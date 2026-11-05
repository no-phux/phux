//! Attached input admission, shared with the Terminal's execution-time
//! recheck. The latter holds the state lock through delivery after a lane wait.

use phux_protocol::wire::frame::{ErrorCode, FrameKind};

use super::ResourceFrame;
use crate::runtime::apps::FrameAdmission;
use crate::state::{ClientId, Outbound, ServerState};

/// Resolve and recheck attached input while the caller holds the delivery
/// lock. No app or engine is selected here.
pub(in crate::runtime) fn attached_input_resource(
    s: &ServerState,
    client: ClientId,
    wire: &phux_protocol::ids::ResourceId,
) -> Option<(phux_core::ids::ResourceId, crate::resource::ResourceHandle)> {
    if !input_connection_live(s, client) {
        return None;
    }
    let crate::state::Resolved::Local(local) = s.resolve_resource(wire) else {
        return None;
    };
    attached_input_allowed(s, client, local.id).then(|| (local.id, local.handle.clone()))
}

pub(super) fn admit(
    ctx: &ResourceFrame<'_>,
    admission: FrameAdmission,
    resource: phux_core::ids::ResourceId,
) -> bool {
    if matches!(admission, FrameAdmission::Unrestricted) {
        return true;
    }
    ctx.state.with(|s| {
        if !input_connection_live(s, ctx.client_id) {
            return false;
        }
        if matches!(admission, FrameAdmission::Subscription) {
            return s
                .subscribers_for_resource(resource)
                .contains(&ctx.client_id);
        }
        attached_input_allowed(s, ctx.client_id, resource)
    })
}

/// Revocation precedes resource lookup and every other input check.
pub(in crate::runtime) fn input_connection_live(s: &ServerState, client: ClientId) -> bool {
    !s.connection_revoked(client)
}

/// Keep the original order: native guardian, retained exit, subscription,
/// then input lease. Neither a lease nor kind refusal may bypass membership.
pub(in crate::runtime) fn attached_input_allowed(
    s: &ServerState,
    client: ClientId,
    resource: phux_core::ids::ResourceId,
) -> bool {
    if s.native_owned_terminal(resource) {
        if let Some(out) = s.client_mailbox(client) {
            let _ = out.try_send(Outbound::Frame(FrameKind::Error {
                request_id: None,
                code: ErrorCode::InputNotWritten,
                message: "SDK-owned guardian refuses terminal input; use native agent commands"
                    .to_owned(),
            }));
        }
        return false;
    }
    if s.retained_exit(resource).is_some() {
        return false;
    }
    if !s.subscribers_for_resource(resource).contains(&client) {
        return false;
    }
    !s.input_blocked(resource, client)
}
