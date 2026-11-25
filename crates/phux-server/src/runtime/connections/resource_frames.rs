//! Connection admission and routing for reply-less resource frames. Apps see
//! only their own local resources; absent and satellite ids stay in the host.

mod clipboard_read;
mod input;
mod relay;
mod terminal;
#[cfg(test)]
mod tests;

pub(in crate::runtime) use input::attached_input_resource;

use phux_protocol::caps::{BootstrapProfile, EngineCodec};
use phux_protocol::wire::frame::{FrameKind, HistoryTombstoneReason};

use super::dispatch::{FrameFlow, direction_invalid};
use super::frames::frame_resource;
use crate::runtime::apps::ConnectionLane;
use crate::runtime::apps::{FrameAdmission, app_for};
use crate::runtime::connections::hello::NegotiatedConnection;
use crate::runtime::connections::plumbing::ClientPlumbing;
use crate::state::{ClientId, SharedState};
use crate::state::{Outbound, ResolvedOwned};
use tokio_util::sync::CancellationToken;
/// One client's stream or input frame about a resource, with what the
/// connection it arrived on lends the app to serve it.
pub(crate) struct ResourceFrame<'a> {
    /// Server state.
    pub(crate) state: &'a SharedState,
    /// The sending connection.
    pub(crate) client_id: ClientId,
    /// The connection's cancellation.
    pub(crate) token: &'a CancellationToken,
    /// The connection's place on the input lane, when it has one.
    pub(crate) lane: Option<&'a mut ConnectionLane>,
    /// The connection's mailboxes: control, and a bound stream per resource.
    pub(crate) plumbing: &'a ClientPlumbing,
    /// What `HELLO` negotiated, once it has.
    pub(crate) negotiated: Option<NegotiatedConnection>,
}

/// Route only after the production policy guard has authorized the frame.
pub(super) async fn serve(ctx: ResourceFrame<'_>, frame: FrameKind) -> FrameFlow {
    let Some(wire) = frame_resource(&frame) else {
        return direction_invalid(ctx.client_id, &frame);
    };
    let resolved = ctx.state.with(|s| s.resolve_resource(wire).into_owned());
    match resolved {
        ResolvedOwned::Local(local) => {
            let app = app_for(local.handle.kind);
            let admission = app.frame_admission(&frame);
            // Using a view changes its active-size vote even when a lease,
            // retained exit, or guardian blocks delivery. Authorization has
            // already run; activity must precede these delivery checks.
            if matches!(admission, FrameAdmission::AttachedInput) {
                note_activity(&ctx, &frame);
            }
            if !input::admit(&ctx, admission, local.id) {
                return FrameFlow::Continue(());
            }
            if let Some((code, message)) = app.frame_error(&frame) {
                if local.handle.kind == phux_core::ResourceKind::AgentSession {
                    let _ = ctx
                        .plumbing
                        .out_tx
                        .try_send(Outbound::Frame(FrameKind::Error {
                            request_id: None,
                            code,
                            message,
                        }));
                } else {
                    crate::runtime::send_error(&ctx.plumbing.out_tx, code, &message).await;
                }
                FrameFlow::Continue(())
            } else {
                terminal::serve_frame(ctx, frame).await
            }
        }
        ResolvedOwned::Remote(route) => {
            // A reply and a history request have never relayed. In particular,
            // do not expose a satellite's history through its shared identity.
            if let Some(frame) = relay::forward(&ctx, &route, frame) {
                return unavailable(&ctx, frame).await;
            }
            FrameFlow::Continue(())
        }
        ResolvedOwned::Unknown => {
            note_activity(&ctx, &frame);
            unavailable(&ctx, frame).await
        }
    }
}

/// Structured input changes the active view independently of delivery
/// admission. Remote and unknown ids retain the same activity accounting.
fn note_activity(ctx: &ResourceFrame<'_>, frame: &FrameKind) {
    use phux_protocol::input::{focus::FocusEvent, mouse::MouseAction};
    let used = match frame {
        FrameKind::InputKey { .. } | FrameKind::InputPaste { .. } => true,
        FrameKind::InputFocus { event, .. } => matches!(event, FocusEvent::Gained),
        FrameKind::InputMouse { event, .. } => matches!(event.action, MouseAction::Press),
        _ => false,
    };
    if used {
        crate::runtime::commands::terminal::handle_client_activity(ctx.state, ctx.client_id);
    }
}

/// Preserve the no-resource outcomes without inventing a serving app.
async fn unavailable(ctx: &ResourceFrame<'_>, frame: FrameKind) -> FrameFlow {
    match frame {
        FrameKind::InputTerminalReply { .. } => {}
        FrameKind::HistoryRequest { .. } => unavailable_history(ctx, frame).await,
        _ => {
            tracing::debug!(client_id = ?ctx.client_id, resource = ?frame_resource(&frame), "resource frame has no local destination; dropping");
        }
    }
    FrameFlow::Continue(())
}

/// Unknown and satellite history requests retain the original codec-first
/// tombstone. This is not a relay, even when the satellite has a hub link.
async fn unavailable_history(ctx: &ResourceFrame<'_>, frame: FrameKind) {
    let Some(selection) = ctx.negotiated else {
        return;
    };
    let FrameKind::HistoryRequest {
        terminal_id,
        stream_id,
        bootstrap_id,
        cursor,
        ..
    } = frame
    else {
        return;
    };
    let reason = if matches!(
        selection.profile,
        Some(BootstrapProfile::NativeState {
            codec: EngineCodec::LibghosttySnapshotV1,
            ..
        })
    ) {
        HistoryTombstoneReason::Released
    } else {
        HistoryTombstoneReason::CodecFailure
    };
    let _ = ctx
        .plumbing
        .sender_for(&terminal_id)
        .send(Outbound::Frame(FrameKind::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
        }))
        .await;
}
