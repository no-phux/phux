//! Translation of wire frames into session-kernel inputs and the
//! kernel-effect route (`KernelRoute`) the handler folds back in.

use std::collections::{HashMap, HashSet};

use phux_client_core::engine::CanonicalGeometry;
use phux_client_core::history::HistoryLoadState;
use phux_client_core::session::{
    AgentSessionDeclaration, EffectBuffer as KernelEffectBuffer,
    HistoryRejectionReason as KernelHistoryRejectionReason, HistoryUnavailableReason, KernelEffect,
    KernelInput, KernelSend,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    AgentEvent, FrameKind, HistoryRejectionReason, HistoryTombstoneReason,
};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};
use phux_protocol::{BootstrapId, ResourceKind, StreamId};

use crate::render::chrome::status_bar::Notice;

use super::outcome::pane_label;

#[derive(Default)]
pub(super) struct KernelRoute {
    pub(super) ack: Option<(ResourceId, StreamId, BootstrapId, u64)>,
    pub(super) history_request: Option<(ResourceId, StreamId, BootstrapId, bytes::Bytes, u32, u32)>,
    pub(super) damaged: HashSet<ResourceId>,
    /// `AgentSession` resources whose record log grew under this frame. The
    /// chrome projects them, so the handler raises a chrome repaint.
    pub(super) agent_touched: HashSet<ResourceId>,
    /// A live-spawned `AgentSession` child first declared by this frame, for
    /// the driver to attach as a record stream.
    pub(super) declared_agent: Option<ResourceId>,
    pub(super) resync_required: bool,
    pub(super) ignored: bool,
    pub(super) failed: Option<String>,
    /// Notices raised by the kernel's own effects.
    pub(super) notices: Vec<Notice>,
    /// Per-pane progressive-history health this frame established: `true`
    /// when the kernel reported history unavailable, `false` when a fresh
    /// replica published or its cache reported a healthy state. Last word
    /// per pane wins; panes absent here keep their flag.
    pub(super) history_degraded: HashMap<ResourceId, bool>,
    /// OSC 52 writes from live output (ADR-0158), for the driver's policy.
    pub(super) clipboard_writes: Vec<(ResourceId, phux_client_core::engine::ClipboardText)>,
}
impl KernelRoute {
    pub(super) fn damaged(&self, terminal_id: &ResourceId) -> bool {
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

/// Whether a snapshot entry is a Terminal-kind resource: the only kind that
/// owns a grid, a pane slot, and a layout leaf.
pub(super) const fn is_terminal(info: &ResourceInfo) -> bool {
    matches!(info.kind, ResourceKind::Terminal)
}

/// The `AgentSession` resources bound to one of `participants`: declared as
/// record streams, never slots or barrier participants.
pub(super) fn attach_agent_sessions<'a>(
    snapshot: &'a SessionSnapshot,
    participants: &[ResourceId],
) -> Vec<&'a ResourceInfo> {
    snapshot
        .resources
        .iter()
        .filter(|info| matches!(info.kind, ResourceKind::AgentSession))
        .filter(|info| {
            info.parent
                .as_ref()
                .is_some_and(|parent| participants.contains(parent))
        })
        .collect()
}

/// The panes an ATTACH will bootstrap: the focused session's (all its
/// windows), Terminal-kind only. The snapshot is a whole-workspace view, but
/// counting other sessions' panes left `ATTACH_READY` unresolvable whenever a
/// second session existed. A pane whose window is not listed is excluded, so
/// the failure mode is a late paint rather than an attach that never completes.
pub(in crate::attach) fn attach_participants(snapshot: &SessionSnapshot) -> Vec<ResourceId> {
    let focused_windows: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == snapshot.focused_session)
        .map(|window| window.id)
        .collect();
    snapshot
        .resources
        .iter()
        .filter(|pane| is_terminal(pane))
        .filter(|pane| focused_windows.contains(&pane.window_id))
        .map(|pane| pane.id.clone())
        .collect()
}

/// Route one inbound wire frame through the session kernel and fold its
/// declarative effects into a [`KernelRoute`] the handler's arm consumes.
pub(super) fn route_engine_frame(
    frame: &FrameKind,
    kernel: &mut crate::attach::pane_state::AttachKernel,
    effects: &mut KernelEffectBuffer,
) -> KernelRoute {
    let terminals = frame_attach_participants(frame);
    let input = match kernel_input_for(frame, &terminals) {
        Ok(Some(input)) => input,
        // A frame the kernel does not model, except a spawn announcement of
        // a new `AgentSession` child, which it must learn about.
        Ok(None) => {
            let mut route = KernelRoute::default();
            declare_spawned_agent_session(frame, kernel, effects, &mut route);
            return route;
        }
        Err(failed) => {
            return KernelRoute {
                failed: Some(failed.to_owned()),
                ..KernelRoute::default()
            };
        }
    };

    effects.clear();
    let result = kernel.update(input, effects);
    let mut route = classify_kernel_result(&result, frame, effects);
    collect_route_effects(&mut route, effects);
    if route.failed.is_none()
        && let FrameKind::Attached { snapshot, .. } = frame
    {
        declare_agent_sessions(snapshot, &terminals, kernel, effects, &mut route);
    }
    if route.failed.is_none() {
        declare_spawned_agent_session(frame, kernel, effects, &mut route);
    }
    note_fresh_history(frame, &mut route);
    route
}

/// Whether a history cache in `state` can still page scrollback in.
const fn history_healthy(state: HistoryLoadState) -> bool {
    matches!(
        state,
        HistoryLoadState::Idle | HistoryLoadState::Loading | HistoryLoadState::Complete
    )
}

/// An accepted `BOOTSTRAP_READY` publishes a replica with a fresh history
/// cache, so an earlier generation's degraded mark no longer applies. A
/// failure reported by the same frame keeps its word.
fn note_fresh_history(frame: &FrameKind, route: &mut KernelRoute) {
    let FrameKind::BootstrapReady { terminal_id, .. } = frame else {
        return;
    };
    if route.failed.is_some() || route.resync_required || route.ignored {
        return;
    }
    route
        .history_degraded
        .entry(terminal_id.clone())
        .or_insert(false);
}

/// Declare a live-spawned `AgentSession` whose parent is a Terminal this
/// kernel holds and that is not yet known.
fn declare_spawned_agent_session(
    frame: &FrameKind,
    kernel: &mut crate::attach::pane_state::AttachKernel,
    effects: &mut KernelEffectBuffer,
    route: &mut KernelRoute,
) {
    let FrameKind::Event {
        terminal: Some(terminal_id),
        event:
            AgentEvent::ResourceSpawned {
                kind: ResourceKind::AgentSession,
                parent: Some(parent),
            },
        ..
    } = frame
    else {
        return;
    };
    if kernel.resource_kind(terminal_id).is_some()
        || kernel.resource_kind(parent) != Some(ResourceKind::Terminal)
    {
        return;
    }
    let declaration = AgentSessionDeclaration {
        terminal_id,
        parent: Some(parent),
        provider: None,
        native_id: None,
        state: None,
    };
    effects.clear();
    match kernel.update(KernelInput::AgentSessionDeclared(declaration), effects) {
        Ok(()) => {
            route.declared_agent = Some(terminal_id.clone());
            collect_route_effects(route, effects);
        }
        Err(error) => route.failed = Some(error.to_string()),
    }
}

/// Declare every `AgentSession` child of an attach participant.
fn declare_agent_sessions(
    snapshot: &SessionSnapshot,
    participants: &[ResourceId],
    kernel: &mut crate::attach::pane_state::AttachKernel,
    effects: &mut KernelEffectBuffer,
    route: &mut KernelRoute,
) {
    for info in attach_agent_sessions(snapshot, participants) {
        let facet = info.agent.as_ref();
        let declaration = AgentSessionDeclaration {
            terminal_id: &info.id,
            parent: info.parent.as_ref(),
            provider: facet.map(|facet| facet.provider.as_str()),
            native_id: facet.and_then(|facet| facet.native_id.as_deref()),
            state: facet.map(|facet| facet.state.as_str()),
        };
        effects.clear();
        if let Err(error) = kernel.update(KernelInput::AgentSessionDeclared(declaration), effects) {
            route.failed = Some(error.to_string());
            return;
        }
        collect_route_effects(route, effects);
    }
}

/// The attach participants an `ATTACHED` frame declares (materialized so
/// `KernelInput::AttachStarted` can borrow it).
fn frame_attach_participants(frame: &FrameKind) -> Vec<ResourceId> {
    let FrameKind::Attached { snapshot, .. } = frame else {
        return Vec::new();
    };
    attach_participants(snapshot)
}

/// Translate a wire frame into its kernel input. `Ok(None)` ⇒ not modeled;
/// `Err` ⇒ an unknown history reason, reported as a rejected route.
fn kernel_input_for<'a>(
    frame: &'a FrameKind,
    terminals: &'a [ResourceId],
) -> Result<Option<KernelInput<'a>>, &'static str> {
    if let Some(input) = attach_stream_input(frame, terminals) {
        return Ok(Some(input));
    }
    if let Some(input) = content_stream_input(frame) {
        return Ok(Some(input));
    }
    history_stream_input(frame)
}

/// The attach barrier and bootstrap-transcript frames.
fn attach_stream_input<'a>(
    frame: &'a FrameKind,
    terminals: &'a [ResourceId],
) -> Option<KernelInput<'a>> {
    match frame {
        FrameKind::Attached { attach_id, .. } => Some(KernelInput::AttachStarted {
            attach_id: *attach_id,
            terminals,
        }),
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
        _ => None,
    }
}

/// The live-content frames: applied VT bytes and the pane's permanent close.
fn content_stream_input(frame: &FrameKind) -> Option<KernelInput<'_>> {
    match frame {
        FrameKind::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => Some(KernelInput::ResourceOutput {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            seq: *seq,
            payload: bytes,
        }),
        FrameKind::ResourceClosed {
            terminal_id,
            exit_status,
            reason,
            signal,
        } => Some(KernelInput::ResourceClosed {
            terminal_id,
            exit_status: *exit_status,
            signal: *signal,
            reason: *reason,
        }),
        _ => None,
    }
}

/// The scrollback-page frames; an unrecognised reason is a rejected route.
fn history_stream_input(frame: &FrameKind) -> Result<Option<KernelInput<'_>>, &'static str> {
    let input = match frame {
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            rows,
            page_seq,
            cursor,
            next_cursor,
            payload,
        } => KernelInput::HistoryPage {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            rows: *rows,
            page_seq: *page_seq,
            payload,
            cursor,
            next_cursor: next_cursor.as_deref(),
        },
        FrameKind::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
        } => KernelInput::HistoryTombstone {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            cursor,
            reason: history_unavailable_reason(*reason)
                .ok_or("unsupported history tombstone reason")?,
        },
        FrameKind::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
            required_bytes,
            required_rows,
        } => KernelInput::HistoryRejected {
            terminal_id,
            stream_id: *stream_id,
            bootstrap_id: *bootstrap_id,
            cursor,
            reason: history_rejection_reason(*reason)
                .ok_or("unsupported history rejection reason")?,
            required_bytes: *required_bytes,
            required_rows: *required_rows,
        },
        _ => return Ok(None),
    };
    Ok(Some(input))
}

/// Did the kernel emit a typed resync-required status alongside its error?
fn emitted_resync_required(effects: &KernelEffectBuffer) -> bool {
    effects.as_slice().iter().any(|effect| {
        matches!(
            effect,
            KernelEffect::Status(phux_client_core::session::KernelStatus::ResyncRequired { .. })
        )
    })
}

/// Did the kernel emit a (recoverable) per-pane history-unavailable status?
fn emitted_history_unavailable(effects: &KernelEffectBuffer) -> bool {
    effects.as_slice().iter().any(|effect| {
        matches!(
            effect,
            KernelEffect::Status(
                phux_client_core::session::KernelStatus::HistoryUnavailable { .. }
            )
        )
    })
}

/// Fold the kernel's `update` result and statuses into the route's verdict.
/// A resync, a retired generation, and a recovered history failure are
/// non-fatal; anything else rejects the frame.
fn classify_kernel_result<E>(
    result: &Result<(), phux_client_core::session::KernelError<E>>,
    frame: &FrameKind,
    effects: &KernelEffectBuffer,
) -> KernelRoute
where
    phux_client_core::session::KernelError<E>: std::fmt::Display,
{
    let resync_required = result.is_err() && emitted_resync_required(effects);
    let recovered_history_failure = result.is_err()
        && matches!(frame, FrameKind::HistoryPage { .. })
        && emitted_history_unavailable(effects);
    let ignored = matches!(
        result,
        Err(phux_client_core::session::KernelError::RetiredGeneration { .. })
    );
    let degraded = resync_required || ignored || recovered_history_failure;
    let failed = match result {
        Ok(()) => None,
        Err(_) if degraded => None,
        Err(error) => Some(error.to_string()),
    };
    KernelRoute {
        resync_required,
        ignored,
        failed,
        ..KernelRoute::default()
    }
}

/// Fold the kernel's effects into the sends, repaints, and notices the
/// handler reads.
fn collect_route_effects(route: &mut KernelRoute, effects: &KernelEffectBuffer) {
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
            KernelEffect::Damage(damage) => {
                route.damaged.insert(damage.terminal_id.clone());
            }
            KernelEffect::AgentRecords { terminal_id, .. } => {
                route.agent_touched.insert(terminal_id.clone());
            }
            // Per-pane and recoverable; the kernel names the pane.
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
                route.history_degraded.insert(key.terminal_id.clone(), true);
            }
            // A healthy cache (a fresh fetch, or a completed one) clears the
            // pane's degraded mark; a failed state leaves it as reported.
            KernelEffect::Status(phux_client_core::session::KernelStatus::History {
                key,
                status,
            }) => {
                tracing::debug!(terminal_id = ?key.terminal_id, ?status, "history status");
                if history_healthy(status.state) {
                    route
                        .history_degraded
                        .insert(key.terminal_id.clone(), false);
                }
            }
            // Cwd/command-boundary/exit statuses have no TUI chrome yet.
            KernelEffect::Status(
                status @ (phux_client_core::session::KernelStatus::Cwd { .. }
                | phux_client_core::session::KernelStatus::CommandStarted { .. }
                | phux_client_core::session::KernelStatus::CommandFinished { .. }
                | phux_client_core::session::KernelStatus::Exited { .. }),
            ) => {
                tracing::debug!(?status, "session kernel status (no TUI consumer yet)");
            }
            // A resync means the replica diverged: worth a warning.
            KernelEffect::Status(
                status @ phux_client_core::session::KernelStatus::ResyncRequired { .. },
            ) => {
                tracing::warn!(?status, "session kernel status");
            }
            KernelEffect::Status(phux_client_core::session::KernelStatus::Engine {
                key,
                status: phux_client_core::engine::EngineStatus::ClipboardWrite(text),
            }) => {
                route
                    .clipboard_writes
                    .push((key.terminal_id.clone(), text.clone()));
            }
            // Title, bell, and history paging are routine (an agent's spinner
            // retitles its pane several times a second): debug, not warn.
            KernelEffect::Status(status) => {
                tracing::debug!(?status, "session kernel status");
            }
            KernelEffect::Job(job) => {
                tracing::debug!(?job, "session kernel cooperative job");
            }
            // The TUI manages its own SUBSCRIBE_EVENTS; the kernel's duplicate
            // is dropped.
            KernelEffect::Send(KernelSend::SubscribeEvents { .. }) => {
                tracing::trace!(
                    "kernel-emitted SUBSCRIBE_EVENTS superseded by the TUI's own subscription"
                );
            }
            KernelEffect::Send(send) => {
                tracing::warn!(?send, "unexpected synchronous engine send");
            }
        }
    }
}
