//! Carrying Terminals across a graceful upgrade (ADR-0032): each live pane's
//! PTY and replay snapshot at the cut, an engine that re-adopts them in the
//! resumed image. The tree around the panes is the host's.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use futures_util::stream::{FuturesUnordered, StreamExt as _};
use phux_core::ids::ResourceId;
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_terminal_api::actor::TerminalResource as _;
use phux_terminal_api::handoff::{ProcessHandoff, UpgradeHandleRequest};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use phux_terminal::{TerminalActor, TerminalActorBundle};

use crate::resource::ResourceHandle;
use crate::runtime::apps::{KindResume, UpgradeCut};
use crate::runtime::connections::pane_events::PaneEvents;
use crate::runtime::upgrade::UpgradeError;
use crate::state::upgrade_blob::{KindRebuild, RebuildError, ResumeCx};
use crate::state::{ServerState, SharedState};
use crate::upgrade::blob::{PaneBlob, RetainedExitBlob, StateBlob};

/// The PTY handoff of every pane one upgrade carries. Dropping it closes
/// the duplicated masters; the running engines keep their own.
#[derive(Debug, Default)]
pub(crate) struct TerminalCut {
    handoffs: HashMap<ResourceId, ProcessHandoff>,
}

impl TerminalCut {
    /// A cut that already holds `handoffs`.
    #[cfg(test)]
    pub(crate) fn holding(handoffs: HashMap<ResourceId, ProcessHandoff>) -> Box<dyn UpgradeCut> {
        Box::new(Self { handoffs })
    }
}

impl UpgradeCut for TerminalCut {
    fn cut<'a>(
        &'a mut self,
        state: &SharedState,
        deadline: Duration,
    ) -> LocalBoxFuture<'a, Result<(), UpgradeError>> {
        let panes = state.with(pane_senders);
        async move {
            self.handoffs = collect_pane_handoffs(panes, deadline).await?;
            Ok(())
        }
        .boxed_local()
    }

    fn carry(&self, state: &ServerState, blob: &mut StateBlob) -> Result<(), UpgradeError> {
        carry_handoffs(state, blob, &self.handoffs);
        Ok(())
    }

    /// The masters this cut duplicated that `blob` names: a retained pane's
    /// or a closed one's stays behind.
    fn inherited_fds(&self, blob: &StateBlob) -> Vec<RawFd> {
        self.handoffs
            .values()
            .filter_map(|handoff| handoff.master_fd.as_ref().map(AsRawFd::as_raw_fd))
            .filter(|fd| blob.panes.iter().any(|pane| pane.master_fd == Some(*fd)))
            .collect()
    }

    fn lift(&mut self) -> LocalBoxFuture<'_, ()> {
        // The engines never stopped; there is nothing to reopen.
        Box::pin(std::future::ready(()))
    }
}

/// The upgrade mailbox of every pane a re-exec carries
/// ([`ServerState::upgrade_handles`]).
pub(crate) fn pane_senders(
    s: &ServerState,
) -> Vec<(ResourceId, mpsc::Sender<UpgradeHandleRequest>)> {
    s.upgrade_handles()
        .into_iter()
        .filter_map(|(pane, handle)| Some((pane, handle.terminal().ok()?.upgrade.clone())))
        .collect()
}

/// Ask one pane's actor for its handoff. Only live tree Terminals are asked,
/// so a closed upgrade mailbox means a pane whose engine already exited and
/// whose exit watcher has not reaped it yet: [`UpgradeError::PaneExiting`],
/// which a retry clears. Carrying it would resurrect a killed pane as a
/// blank one that never exits. Any failure after the request was accepted
/// (a dropped reply, a half PTY pair, the deadline) means a live actor could
/// not hand off.
pub(crate) async fn request_pane_handoff(
    pane: ResourceId,
    upgrade: &mpsc::Sender<UpgradeHandleRequest>,
) -> Result<ProcessHandoff, UpgradeError> {
    let (reply, rx) = oneshot::channel();
    if upgrade.send(UpgradeHandleRequest { reply }).await.is_err() {
        tracing::warn!(
            ?pane,
            "upgrade: pane engine already exited and is awaiting its reap; aborting for retry"
        );
        return Err(UpgradeError::PaneExiting { pane });
    }
    rx.await.map_err(|_| UpgradeError::PaneHandoff {
        pane,
        reason: "actor dropped its reply",
    })
}

/// Every pane's handoff within one aggregate `deadline`. A pane that cannot
/// hand off fails the upgrade: its PTY would be stranded.
pub(crate) async fn collect_pane_handoffs(
    handles: Vec<(ResourceId, mpsc::Sender<UpgradeHandleRequest>)>,
    deadline: Duration,
) -> Result<HashMap<ResourceId, ProcessHandoff>, UpgradeError> {
    tokio::time::timeout(deadline, async move {
        let pane_count = handles.len();
        let mut pending = handles
            .into_iter()
            .map(|(pane, sender)| async move {
                request_pane_handoff(pane, &sender)
                    .await
                    .map(|handoff| (pane, handoff))
            })
            .collect::<FuturesUnordered<_>>();
        let mut handoffs = HashMap::with_capacity(pane_count);
        while let Some(result) = pending.next().await {
            let (pane, handoff) = result?;
            let pair_is_valid = matches!(
                (&handoff.master_fd, handoff.child_pid),
                (Some(_), Some(1..)) | (None, None)
            );
            if !pair_is_valid {
                return Err(UpgradeError::InvalidPaneHandoff { pane });
            }
            handoffs.insert(pane, handoff);
        }
        Ok(handoffs)
    })
    .await
    .map_err(|_| UpgradeError::HandoffDeadline)?
}

/// Fill each live pane in `blob` with its engine's handoff: live size,
/// PTY, replay, and the program's title. A retained pane's PTY does not
/// cross (ADR-0124 §6), and a pane without a handoff keeps the registry's
/// values.
pub(crate) fn carry_handoffs(
    s: &ServerState,
    blob: &mut StateBlob,
    handoffs: &HashMap<ResourceId, ProcessHandoff>,
) {
    for pane in blob.panes.iter_mut().filter(|p| p.retained_exit.is_none()) {
        let handoff = s
            .resource_from_wire(&WireResourceId::local(pane.wire_id))
            .and_then(|id| handoffs.get(&id));
        if let Some(handoff) = handoff {
            apply_handoff(pane, handoff);
        }
    }
}

fn apply_handoff(pane: &mut PaneBlob, h: &ProcessHandoff) {
    (pane.cols, pane.rows) = (h.cols, h.rows);
    pane.cell_px = h.cell_px;
    if let Some(cwd) = h.cwd.as_deref() {
        pane.cwd = PathBuf::from(cwd);
    }
    pane.osc_title.clone_from(&h.osc_title);
    pane.child_pid = h.child_pid;
    pane.master_fd = h.master_fd.as_ref().map(AsRawFd::as_raw_fd);
    pane.vt_replay_bytes.clone_from(&h.vt_replay_bytes);
    pane.scrollback_bytes.clone_from(&h.scrollback_bytes);
}

/// One pane rebuilt around its inherited state, ready to register.
pub(crate) struct RebuiltPane<W> {
    /// The engine's handle.
    pub(crate) handle: ResourceHandle,
    /// The engine's cancellation token.
    pub(crate) token: CancellationToken,
    /// The engine, not yet running.
    pub(crate) engine: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>,
    /// Fires when the engine's child exits.
    pub(crate) exit_notify: Option<oneshot::Receiver<phux_core::ExitOutcome>>,
    /// What the caller's wiring returned.
    pub(crate) wired: W,
}

/// What a rebuilt pane needs once the tree is committed.
pub(crate) trait PaneStart {
    /// Start the pane's runtime wiring, handing back its events.
    fn start(self, state: &SharedState, wire: &WireResourceId) -> Option<PaneEvents>;
}

impl PaneStart for super::commands::PaneWiring {
    fn start(self, state: &SharedState, wire: &WireResourceId) -> Option<PaneEvents> {
        Some(Self::start(self, state, wire))
    }
}

impl PaneStart for () {
    fn start(self, _state: &SharedState, _wire: &WireResourceId) -> Option<PaneEvents> {
        None
    }
}

/// Rebuilds every carried pane through `build`, keeping what it wired.
pub(crate) struct TerminalResume<B, W> {
    build: B,
    /// Each rebuilt pane and its wiring, until [`KindResume::start`].
    pub(crate) wired: Vec<(ResourceId, W)>,
}

impl<B, W> TerminalResume<B, W>
where
    B: FnMut(
        ResourceId,
        &PaneBlob,
        phux_config::ScrollbackLimits,
    ) -> Result<RebuiltPane<W>, RebuildError>,
{
    pub(crate) const fn new(build: B) -> Self {
        Self {
            build,
            wired: Vec::new(),
        }
    }
}

/// The resume a server runs: each pane wired as a fresh spawn is.
pub(crate) fn resume(state: &SharedState) -> Box<dyn KindResume> {
    Box::new(TerminalResume::new(wired(state.clone())))
}

impl<B, W> KindRebuild for TerminalResume<B, W>
where
    B: FnMut(
        ResourceId,
        &PaneBlob,
        phux_config::ScrollbackLimits,
    ) -> Result<RebuiltPane<W>, RebuildError>,
{
    /// Bind every carried pane's wire id to its record's new handle and
    /// spawn the engine that re-adopts its PTY. A pane is the tree's own, so
    /// one that cannot be rebuilt fails the resume.
    fn rebuild(&mut self, cx: &mut ResumeCx<'_>) -> Result<(), RebuildError> {
        let scrollback = cx.state.scrollback_limits();
        let blob = cx.blob;
        for p in &blob.panes {
            let core = cx.record(p.wire_id).ok_or(RebuildError::DanglingRef {
                kind: "pane record",
                id: p.wire_id,
            })?;
            if let Some(desc) = cx.state.registry_mut().terminal_mut(core) {
                desc.dims = (p.cols, p.rows);
                desc.cwd.clone_from(&p.cwd);
                desc.title.clone_from(&p.title);
                // The engine is seeded with this title without emitting a
                // change, so the registry takes it from the blob directly.
                desc.program_title.clone_from(&p.osc_title);
            }
            let RebuiltPane {
                handle,
                token,
                engine,
                exit_notify,
                wired,
            } = (self.build)(core, p, scrollback)?;
            self.wired.push((core, wired));
            cx.spawn(core, p.wire_id, handle, token, engine, exit_notify)?;
            // ADR-0124: what the old image knew about this pane's retention.
            if let Some(secs) = p.retain_secs {
                cx.state.note_retain_request(core, secs);
            }
            if let Some(exit) = p.retained_exit {
                cx.state.restore_retained_exit(core, exit_facet(exit));
            }
        }
        Ok(())
    }

    fn restore(&mut self, _state: &mut ServerState, _blob: &StateBlob) {}
}

impl<B, W> KindResume for TerminalResume<B, W>
where
    B: FnMut(
        ResourceId,
        &PaneBlob,
        phux_config::ScrollbackLimits,
    ) -> Result<RebuiltPane<W>, RebuildError>,
    W: PaneStart,
{
    fn start(&mut self, state: &SharedState) -> Vec<(ResourceId, PaneEvents)> {
        std::mem::take(&mut self.wired)
            .into_iter()
            .filter_map(|(pane, wiring)| {
                let wire = state.with_mut(|s| s.intern_resource_wire(pane));
                Some((pane, wiring.start(state, &wire)?))
            })
            .collect()
    }
}

/// The exit facet a resumed image restores from the blob.
const fn exit_facet(blob: RetainedExitBlob) -> phux_protocol::wire::info::ExitFacet {
    phux_protocol::wire::info::ExitFacet::new(blob.exited_at_ms, blob.retained_until_ms)
        .with_exit_status(blob.exit_status)
        .with_signal(blob.signal)
}

/// Concatenate a pane's scrollback then viewport replay — the order a client
/// applies them, so seeding a fresh `Terminal` reproduces the same grid —
/// then the program's OSC title as an `OSC 2`, so the rebuilt engine reports
/// the title the program set rather than none.
fn pane_seed(p: &PaneBlob) -> Vec<u8> {
    let mut seed = Vec::with_capacity(p.scrollback_bytes.len() + p.vt_replay_bytes.len());
    seed.extend_from_slice(&p.scrollback_bytes);
    seed.extend_from_slice(&p.vt_replay_bytes);
    if let Some(title) = p.osc_title.as_deref() {
        seed.extend_from_slice(&osc_title_sequence(title));
    }
    seed
}

/// `OSC 2 ; title ST`, with control characters dropped so a title can
/// neither terminate the sequence early nor smuggle another one in.
fn osc_title_sequence(title: &str) -> Vec<u8> {
    let clean: String = title.chars().filter(|c| !c.is_control()).collect();
    let mut seq = Vec::with_capacity(clean.len() + 6);
    seq.extend_from_slice(b"\x1b]2;");
    seq.extend_from_slice(clean.as_bytes());
    seq.extend_from_slice(b"\x1b\\");
    seq
}

/// Build a pane actor around its inherited PTY, or a PTY-less actor that
/// replays its snapshot. Only one of the two PTY ids is corruption.
#[cfg(test)]
fn pane_actor_bundle(
    p: &PaneBlob,
    scrollback: phux_config::ScrollbackLimits,
) -> Result<TerminalActorBundle, RebuildError> {
    let seed = pane_seed(p);
    let boxed = |error| RebuildError::Actor(Box::new(error));
    Ok(match (p.master_fd, p.child_pid) {
        (Some(master_fd), Some(child_pid)) => TerminalActor::new_with_adopted_pty(
            master_fd,
            child_pid,
            p.cols,
            p.rows,
            scrollback,
            CancellationToken::new(),
            &seed,
        )
        .map_err(boxed)?,
        (None, None) => TerminalActor::new_with_seed(p.cols, p.rows, &seed).map_err(boxed)?,
        _ => {
            return Err(RebuildError::InvalidPtyPair { wire_id: p.wire_id });
        }
    })
}

/// Rebuild one pane, letting `wire` install its sinks before the actor runs.
#[cfg(test)]
pub(crate) fn rebuild_pane<W>(
    core: ResourceId,
    p: &PaneBlob,
    scrollback: phux_config::ScrollbackLimits,
    wire: impl FnOnce(ResourceId, &mut TerminalActor) -> W,
) -> Result<RebuiltPane<W>, RebuildError> {
    let TerminalActorBundle {
        mut actor,
        handle,
        token,
        exit_notify,
        ..
    } = pane_actor_bundle(p, scrollback)?;
    let wired = wire(core, &mut actor);
    Ok(RebuiltPane {
        handle,
        token,
        engine: Box::pin(actor.run()),
        exit_notify,
        wired,
    })
}

/// A resumed pane is wired exactly as a fresh spawn is (event sink, agent
/// detector), so it keeps its events and agent-state feed. The engine
/// factory is read here, before the rebuild takes the state lock.
fn wired(
    state: SharedState,
) -> impl FnMut(
    ResourceId,
    &PaneBlob,
    phux_config::ScrollbackLimits,
) -> Result<RebuiltPane<super::commands::PaneWiring>, RebuildError>
+ 'static {
    let engine = state.terminal_engine();
    move |core, p, scrollback| {
        let source = match (p.master_fd, p.child_pid) {
            (Some(master_fd), Some(child_pid)) => phux_terminal_api::EngineSource::Adopt {
                master_fd,
                child_pid,
                seed: pane_seed(p),
            },
            (None, None) => phux_terminal_api::EngineSource::Replay(pane_seed(p)),
            _ => return Err(RebuildError::InvalidPtyPair { wire_id: p.wire_id }),
        };
        let reports = crate::runtime::apps::terminal::commands::spawn::engine_reports(&state, core);
        let engine = engine.as_ref().ok_or_else(|| {
            RebuildError::Actor(Box::new(phux_terminal_api::TerminalActorError::Refused(
                "no Terminal engine supplied".into(),
            )))
        })?;
        let bundle = engine
            .build(phux_terminal_api::EngineBuild {
                cols: p.cols,
                rows: p.rows,
                source,
                scrollback,
                token: CancellationToken::new(),
                default_colors: None,
                reports,
            })
            .map_err(|error| RebuildError::Actor(Box::new(error)))?;
        Ok(RebuiltPane {
            handle: bundle.handle,
            token: bundle.token,
            engine: bundle.run,
            exit_notify: bundle.exit_notify,
            wired: super::commands::PaneWiring::new(
                bundle.events,
                bundle.agent_state,
                bundle.program_status,
                bundle.resume,
            ),
        })
    }
}

/// A rebuild with no wiring, for tests of the tree alone.
#[cfg(test)]
pub(crate) fn unwired(
    core: ResourceId,
    p: &PaneBlob,
    scrollback: phux_config::ScrollbackLimits,
) -> Result<RebuiltPane<()>, RebuildError> {
    rebuild_pane(core, p, scrollback, |_, _| ())
}

#[cfg(test)]
mod tests {
    /// A replayed title cannot break out of its `OSC 2`.
    #[test]
    fn the_replayed_title_drops_control_characters() {
        assert_eq!(
            super::osc_title_sequence("a\x07b\x1b]0;c"),
            b"\x1b]2;ab]0;c\x1b\\".to_vec()
        );
    }
}
