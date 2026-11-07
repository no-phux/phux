//! Satellite routing is a host concern, before local app selection.

use phux_protocol::wire::frame::FrameKind;

use super::{ResourceFrame, frame_resource};
use crate::runtime::commands::satellite_lease::relay_satellite_frame;
use crate::state::RelayRoute;

/// Consume a relayable frame (including a refused relay), or return one the
/// host must handle locally. Moving the payload avoids copying a large paste.
pub(super) fn forward(
    ctx: &ResourceFrame<'_>,
    route: &RelayRoute,
    mut frame: FrameKind,
) -> Option<FrameKind> {
    let Some(wire) = frame_resource(&frame).cloned() else {
        return Some(frame);
    };
    let input = structured_input(&frame);
    if input {
        super::note_activity(ctx, &frame);
    }
    if !rewrite(&mut frame, route.local_wire_id()) {
        return Some(frame);
    }
    if input && !input_allowed(ctx, route) {
        return None;
    }
    if !relay_satellite_frame(ctx.client_id, &wire, route, "RESOURCE_FRAME", |_| frame) {
        tracing::warn!(client_id = ?ctx.client_id, ?wire, "resource frame has no satellite relay; dropping");
    }
    None
}

const fn structured_input(frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::InputKey { .. }
            | FrameKind::InputMouse { .. }
            | FrameKind::InputFocus { .. }
            | FrameKind::InputPaste { .. }
    )
}

fn input_allowed(ctx: &ResourceFrame<'_>, route: &RelayRoute) -> bool {
    ctx.state.with(|s| {
        if !s.has_satellite_proxy_attach(ctx.client_id, &route.host, route.id) {
            return false;
        }
        // The satellite sees one hub identity, not these individual consumers.
        s.satellite_lease_holder(&route.host, route.id)
            .is_none_or(|holder| holder == ctx.client_id)
    })
}

fn rewrite(frame: &mut FrameKind, local: phux_protocol::ids::ResourceId) -> bool {
    match frame {
        FrameKind::InputKey { terminal_id, .. }
        | FrameKind::InputMouse { terminal_id, .. }
        | FrameKind::InputFocus { terminal_id, .. }
        | FrameKind::InputPaste { terminal_id, .. }
        | FrameKind::FrameAck { terminal_id, .. } => *terminal_id = local,
        FrameKind::ResizeTerminal {
            terminal_id,
            cell_px,
            ..
        } => {
            *terminal_id = local;
            *cell_px = cell_px.filter(|&(width, height)| width > 0 && height > 0);
        }
        _ => return false,
    }
    true
}
