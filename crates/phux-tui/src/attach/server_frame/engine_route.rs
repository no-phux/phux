//! Translation of wire frames into session-kernel inputs and the
//! kernel-effect route (`KernelRoute`) the handler folds back in.

use std::collections::{HashMap, HashSet};

use phux_client_core::history::HistoryLoadState;
use phux_client_core::session::{
    AgentSessionDeclaration, EffectBuffer as KernelEffectBuffer, KernelEffect, KernelInput,
    KernelSend,
};
use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::{AgentEvent, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{BootstrapId, ResourceKind, StreamId};

use crate::render::chrome::status_bar::Notice;

use super::outcome::pane_label;

mod kernel_input;

use kernel_input::kernel_input_for;

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

/// The panes an ATTACH will bootstrap: the focused session's members,
/// arranged or not, Terminal-kind only. The snapshot is a whole-server view,
/// but counting other sessions' panes left `ATTACH_READY` unresolvable
/// whenever a second session existed. A pane whose session the snapshot
/// cannot name is excluded, so the failure mode is a late paint rather than
/// an attach that never completes.
pub(in crate::attach) fn attach_participants(snapshot: &SessionSnapshot) -> Vec<ResourceId> {
    session_terminals(
        &snapshot.windows,
        &snapshot.resources,
        snapshot.focused_session,
    )
}

/// Every Terminal that belongs to `session`, arranged or not, joined through
/// the same rule as [`phux_client::resource::session_of`].
pub(in crate::attach) fn session_terminals(
    windows: &[WindowInfo],
    resources: &[ResourceInfo],
    session: SessionId,
) -> Vec<ResourceId> {
    resources
        .iter()
        .filter(|pane| is_terminal(pane))
        .filter(|pane| {
            crate::attach::sidebar_zones::resource_session(pane, windows) == Some(session)
        })
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
    // A frame the kernel does not model, except a spawn announcement of a
    // new `AgentSession` child, which it must learn about.
    let Some(input) = kernel_input_for(frame, &terminals) else {
        let mut route = KernelRoute::default();
        declare_spawned_agent_session(frame, kernel, effects, &mut route);
        return route;
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
            KernelEffect::AgentPublication(publication) => {
                route.agent_touched.insert(publication.resource_id.clone());
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
                    "{}: scrollback unavailable",
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
