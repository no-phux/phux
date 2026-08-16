//! Translation of wire frames into session-kernel inputs and the
//! kernel-effect route (`KernelRoute`) the handler folds back in.

use std::collections::HashSet;

use phux_client_core::engine::CanonicalGeometry;
use phux_client_core::session::{
    EffectBuffer as KernelEffectBuffer, HistoryRejectionReason as KernelHistoryRejectionReason,
    HistoryUnavailableReason, KernelEffect, KernelInput, KernelSend,
};
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{FrameKind, HistoryRejectionReason, HistoryTombstoneReason};
use phux_protocol::{BootstrapId, StreamId};

use crate::render::chrome::status_bar::Notice;

use super::outcome::pane_label;

#[derive(Default)]
pub(super) struct KernelRoute {
    pub(super) ack: Option<(TerminalId, StreamId, BootstrapId, u64)>,
    pub(super) history_request: Option<(TerminalId, StreamId, BootstrapId, bytes::Bytes, u32, u32)>,
    pub(super) pty_writes: Vec<(TerminalId, Vec<u8>)>,
    pub(super) damaged: HashSet<TerminalId>,
    pub(super) resync_required: bool,
    pub(super) ignored: bool,
    pub(super) failed: Option<String>,
    /// phux-ijuj: transient status-bar notices raised by the kernel's own
    /// effects rather than by the frame arm. Folded into the dispatched
    /// [`FrameOutcome`] by the arms whose frames can produce them.
    pub(super) notices: Vec<Notice>,
}
impl KernelRoute {
    pub(super) fn damaged(&self, terminal_id: &TerminalId) -> bool {
        self.damaged.contains(terminal_id)
    }
}

pub(super) const fn history_unavailable_reason(
    reason: HistoryTombstoneReason,
) -> Option<HistoryUnavailableReason> {
    Some(match reason {
        HistoryTombstoneReason::Stale => HistoryUnavailableReason::Stale,
        HistoryTombstoneReason::Pruned => HistoryUnavailableReason::Pruned,
        HistoryTombstoneReason::Reset => HistoryUnavailableReason::Reset,
        HistoryTombstoneReason::Resize => HistoryUnavailableReason::Resize,
        HistoryTombstoneReason::Expired => HistoryUnavailableReason::Expired,
        HistoryTombstoneReason::Released => HistoryUnavailableReason::Released,
        HistoryTombstoneReason::Limit => HistoryUnavailableReason::Limit,
        HistoryTombstoneReason::CodecFailure => HistoryUnavailableReason::CodecFailure,
        _ => return None,
    })
}

pub(super) const fn history_rejection_reason(
    reason: HistoryRejectionReason,
) -> Option<KernelHistoryRejectionReason> {
    Some(match reason {
        HistoryRejectionReason::ZeroLimit => KernelHistoryRejectionReason::ZeroLimit,
        HistoryRejectionReason::TooSmall => KernelHistoryRejectionReason::TooSmall,
        HistoryRejectionReason::Busy => KernelHistoryRejectionReason::Busy,
        _ => return None,
    })
}

/// The panes an ATTACH will actually bootstrap: those of the focused session.
///
/// `SessionSnapshot` is a whole-workspace view — its own field docs say
/// `panes` is "every pane across every visible window", because the sidebar
/// and session switcher need the full tree. The **attach** is narrower: the
/// server bootstraps only `attach_snapshot_panes(sid)`, the focused session's
/// panes (`runtime::commands::prepare_attach`).
///
/// Treating every pane in the snapshot as an attach participant therefore
/// registered panes that would never receive a `BOOTSTRAP_*` frame, so they
/// stayed unresolved and `ATTACH_READY` was rejected with
/// `AttachNotReady { remaining }` — where `remaining` was exactly the pane
/// count of the *other* sessions. The practical effect: attaching worked on a
/// server with one session and failed on every server with two or more, which
/// is to say phux stopped working as a multiplexer the moment it was used as
/// one (phux-atch).
///
/// Scoping here rather than narrowing the snapshot keeps the wire contract
/// intact: the client still receives the whole workspace, and only the attach
/// bookkeeping is session-scoped. It spans the session's *windows*: the
/// aggregate barrier covers every pane the server will bootstrap, which is
/// the whole session tree, not just the focused window.
///
/// A pane whose `window_id` has no matching `WindowInfo` is **excluded**. A
/// real snapshot always carries one per window, so this is unreachable in
/// practice; the choice matters only for the direction it fails. Excluding
/// risks releasing the barrier before a pane has bootstrapped (it renders a
/// beat late); including risks an attach that can never complete, which is
/// the failure being fixed here.
pub(super) fn attach_participants(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
) -> Vec<TerminalId> {
    let focused_windows: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == snapshot.focused_session)
        .map(|window| window.id)
        .collect();
    snapshot
        .panes
        .iter()
        .filter(|pane| focused_windows.contains(&pane.window_id))
        .map(|pane| pane.id.clone())
        .collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "cohesive translation of ordered wire-frame variants into session-kernel inputs and effects"
)]
pub(super) fn route_engine_frame(
    frame: &FrameKind,
    kernel: &mut crate::attach::pane_state::AttachKernel,
    effects: &mut KernelEffectBuffer,
) -> KernelRoute {
    let terminals;
    let input = match frame {
        FrameKind::Attached {
            attach_id,
            snapshot,
            ..
        } => {
            terminals = attach_participants(snapshot);
            Some(KernelInput::AttachStarted {
                attach_id: *attach_id,
                terminals: &terminals,
            })
        }
        FrameKind::AttachReady { attach_id } => Some(KernelInput::AttachReady {
            attach_id: *attach_id,
        }),
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => Some(KernelInput::BootstrapBegin {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            profile: *profile,
            geometry: CanonicalGeometry {
                cols: *cols,
                rows: *rows,
            },
            base_seq: *base_seq,
        }),
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => Some(KernelInput::BootstrapChunk {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            chunk_seq: *chunk_seq,
            payload,
        }),
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => Some(KernelInput::BootstrapReady {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            history_cursor: history_cursor.as_deref(),
        }),
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            rows,
            page_seq,
            cursor,
            next_cursor,
            payload,
        } => Some(KernelInput::HistoryPage {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            rows: *rows,
            page_seq: *page_seq,
            payload,
            cursor,
            next_cursor: next_cursor.as_deref(),
        }),
        FrameKind::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
        } => Some(KernelInput::HistoryTombstone {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            cursor,
            reason: match history_unavailable_reason(*reason) {
                Some(reason) => reason,
                None => {
                    return KernelRoute {
                        failed: Some("unsupported history tombstone reason".to_owned()),
                        ..KernelRoute::default()
                    };
                }
            },
        }),
        FrameKind::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
            required_bytes,
            required_rows,
        } => Some(KernelInput::HistoryRejected {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            cursor,
            reason: match history_rejection_reason(*reason) {
                Some(reason) => reason,
                None => {
                    return KernelRoute {
                        failed: Some("unsupported history rejection reason".to_owned()),
                        ..KernelRoute::default()
                    };
                }
            },
            required_bytes: *required_bytes,
            required_rows: *required_rows,
        }),
        FrameKind::TerminalOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => Some(KernelInput::TerminalOutput {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            seq: *seq,
            payload: bytes,
        }),
        FrameKind::BootstrapTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        } => Some(KernelInput::Tombstone {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            reason: *reason,
            last_valid_seq: *last_valid_seq,
        }),
        FrameKind::TerminalClosed { terminal_id, .. } => {
            Some(KernelInput::TerminalClosed { terminal_id })
        }
        _ => None,
    };
    let Some(input) = input else {
        return KernelRoute::default();
    };

    effects.clear();
    let result = kernel.update(input, effects);
    let resync_required = result.is_err()
        && effects.as_slice().iter().any(|effect| {
            matches!(
                effect,
                KernelEffect::Status(
                    phux_client_core::session::KernelStatus::ResyncRequired { .. }
                )
            )
        });
    let recovered_history_failure = result.is_err()
        && matches!(frame, FrameKind::HistoryPage { .. })
        && effects.as_slice().iter().any(|effect| {
            matches!(
                effect,
                KernelEffect::Status(
                    phux_client_core::session::KernelStatus::HistoryUnavailable { .. }
                )
            )
        });
    let ignored = matches!(
        &result,
        Err(phux_client_core::session::KernelError::RetiredGeneration { .. })
    );
    let failed = match result {
        Ok(()) => None,
        Err(_) if resync_required || ignored || recovered_history_failure => None,
        Err(error) => Some(error.to_string()),
    };
    let mut route = KernelRoute {
        resync_required,
        ignored,
        failed,
        ..KernelRoute::default()
    };
    for effect in effects.as_slice() {
        match effect {
            KernelEffect::Send(KernelSend::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            }) => {
                route.ack = Some((terminal_id.clone(), *stream_id, *bootstrap_id, *seq));
            }
            KernelEffect::Send(KernelSend::HistoryRequest {
                key,
                cursor,
                max_bytes,
                max_rows,
            }) => {
                route.history_request = Some((
                    key.terminal_id.clone(),
                    key.stream_id,
                    key.bootstrap_id,
                    bytes::Bytes::from(cursor.clone()),
                    *max_bytes,
                    *max_rows,
                ));
            }
            KernelEffect::Send(KernelSend::PtyWrite { terminal_id, bytes }) => {
                route.pty_writes.push((terminal_id.clone(), bytes.clone()));
            }
            KernelEffect::Damage(damage) => {
                route.damaged.insert(damage.terminal_id.clone());
            }
            // phux-ijuj: history degradation is per-pane and recoverable —
            // the live stream stays valid, only that pane's scrollback
            // boundary is gone. The kernel already told us WHICH pane, so
            // unlike an uncorrelated ERROR this one can name it.
            KernelEffect::Status(phux_client_core::session::KernelStatus::HistoryUnavailable {
                key,
                reason,
            }) => {
                tracing::warn!(
                    terminal_id = ?key.terminal_id,
                    ?reason,
                    "history unavailable for pane"
                );
                route.notices.push(Notice::warn(format!(
                    "{}: scrollback unavailable ({reason:?})",
                    pane_label(&key.terminal_id),
                )));
            }
            KernelEffect::Status(status) => {
                tracing::warn!(?status, "session kernel status");
            }
            KernelEffect::Job(job) => {
                tracing::debug!(?job, "session kernel cooperative job");
            }
            KernelEffect::Send(send) => {
                tracing::warn!(?send, "unexpected synchronous engine send");
            }
        }
    }
    route
}
