//! Connection admission and routing for reply-less resource frames. Apps see
//! only their own local resources; absent and satellite ids stay in the host.

mod input;
mod relay;
#[cfg(all(test, feature = "native-engine", not(target_arch = "wasm32")))]
mod tests;

pub(in crate::runtime) use input::attached_input_resource;

use phux_protocol::caps::{BootstrapProfile, EngineCodec};
use phux_protocol::wire::frame::{ErrorCode, FrameKind, HistoryTombstoneReason};

use super::dispatch::FrameFlow;
use super::frames::frame_resource;
use crate::runtime::apps::terminal::terminal_streams::direction_invalid;
use crate::runtime::apps::{FrameAdmission, ResourceFrame, app_for};
use crate::state::{Outbound, ResolvedOwned};

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
            app.frame(ctx, frame).await
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

/// Structured Terminal input changes the active view independently of delivery
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
        crate::runtime::apps::terminal::commands::handle_client_activity(ctx.state, ctx.client_id);
    }
}

/// Preserve the no-resource outcomes without inventing a serving app.
async fn unavailable(ctx: &ResourceFrame<'_>, frame: FrameKind) -> FrameFlow {
    #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
    if matches!(frame, FrameKind::HistoryRequest { .. }) {
        return direction_invalid(ctx.client_id, &frame);
    }
    match frame {
        FrameKind::InputTerminalReply { .. } => {
            if let Some(selection) = ctx.negotiated
                && !selection.accepts_terminal_reply()
            {
                crate::runtime::send_error(
                    &ctx.plumbing.out_tx,
                    ErrorCode::UnknownMessageType,
                    "INPUT_TERMINAL_REPLY was not advertised for this connection",
                )
                .await;
            }
        }
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
