//! Carrying agent sessions across a graceful upgrade (ADR-0032): sealing their
//! streams at the cut, rolling that back if the upgrade fails, and rebuilding
//! each engine in the resumed image. Dropping what [`begin`] returns unseals
//! the sessions; a successful re-exec replaces the process instead.

use std::collections::{HashMap, HashSet};
use std::os::fd::RawFd;
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use futures_util::stream::{FuturesUnordered, StreamExt as _};
use phux_agent_session::session::AgentSessionResource as _;
use phux_core::ids::ResourceId;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::resource::ResourceHandle;
use crate::runtime::apps::{KindResume, UpgradeCut};
use crate::runtime::connections::pane_events::PaneEvents;
use crate::runtime::upgrade::UpgradeError;
use crate::state::upgrade_blob::{KindRebuild, RebuildError, ResumeCx, live_pane_wires};
use crate::state::{ServerState, SharedState};
use crate::upgrade::blob::{AgentSessionBlob, AgentStreamCut, StateBlob};
use phux_agent_session::session::{AgentSessionActor, AgentSessionBootstrap, BootstrapRequest};

pub(crate) const AGENT_ROLLBACK_TIMEOUT: Duration = Duration::from_secs(2);

/// Stop native input/receipt mutation across the sealed upgrade cut. The
/// durable SDK keeps draining into its bounded output/receipt spool; rollback
/// resumes this broker, while successful exec replaces it with restored state.
struct NativeBrokerSeal(SharedState);

impl NativeBrokerSeal {
    fn new(state: &SharedState) -> Option<Self> {
        let admitted = state.with_mut(|s| {
            let broker = s.native_agents_mut();
            if broker.sealed {
                return false;
            }
            broker.sealed = true;
            true
        });
        admitted.then(|| Self(state.clone()))
    }
}

impl Drop for NativeBrokerSeal {
    fn drop(&mut self) {
        self.0.with_mut(|s| {
            s.native_agents_mut().sealed = false;
            for wake in s.native_agents().wakes.values() {
                wake.notify_waiters();
            }
        });
    }
}

/// Everything one upgrade holds of this kind. Field order is drop order: the
/// streams reopen before the broker does.
pub(crate) struct AgentSessionUpgrade {
    cuts: HashMap<ResourceId, AgentStreamCut>,
    seal: AgentSessionSeal,
    _broker: Option<NativeBrokerSeal>,
}

/// Seal the native broker for an upgrade, or `None` when one already did.
pub(crate) fn begin(state: &SharedState) -> Option<Box<dyn UpgradeCut>> {
    let broker = NativeBrokerSeal::new(state)?;
    Some(Box::new(AgentSessionUpgrade {
        cuts: HashMap::new(),
        seal: AgentSessionSeal::default(),
        _broker: Some(broker),
    }))
}

impl AgentSessionUpgrade {
    /// An upgrade already holding `seal`, with no broker behind it.
    #[cfg(test)]
    pub(crate) fn holding(seal: AgentSessionSeal) -> Box<dyn UpgradeCut> {
        Box::new(Self {
            cuts: HashMap::new(),
            seal,
            _broker: None,
        })
    }

    /// An upgrade that already cut `cuts`, sealing nothing.
    #[cfg(test)]
    pub(crate) fn carrying(cuts: HashMap<ResourceId, AgentStreamCut>) -> Box<dyn UpgradeCut> {
        Box::new(Self {
            cuts,
            seal: AgentSessionSeal::default(),
            _broker: None,
        })
    }
}

impl UpgradeCut for AgentSessionUpgrade {
    fn cut<'a>(
        &'a mut self,
        state: &SharedState,
        deadline: Duration,
    ) -> LocalBoxFuture<'a, Result<(), UpgradeError>> {
        let sessions = state.with(carried_sessions);
        async move {
            let (cuts, seal) = collect_agent_session_cuts(sessions, deadline).await;
            self.cuts = cuts;
            self.seal = seal;
            Ok(())
        }
        .boxed_local()
    }

    /// SDK-owned streams are part of a durable provider session, so unlike
    /// attached streams they may never be dropped at an upgrade cut.
    fn carry(&self, s: &ServerState, blob: &mut StateBlob) -> Result<(), UpgradeError> {
        if let Some(agent) = uncarried(s, &self.cuts) {
            return Err(UpgradeError::NativeAgentHandoff { agent });
        }
        carry_cuts(s, blob, &self.cuts);
        Ok(())
    }

    fn inherited_fds(&self, _blob: &StateBlob) -> Vec<RawFd> {
        Vec::new()
    }

    fn lift(&mut self) -> LocalBoxFuture<'_, ()> {
        self.seal.lift().boxed_local()
    }
}

/// A native agent that cannot be left behind but has no cut in `cuts`.
fn uncarried(
    s: &ServerState,
    cuts: &HashMap<ResourceId, AgentStreamCut>,
) -> Option<phux_protocol::ids::ResourceId> {
    s.native_agents()
        .bindings
        .values()
        .filter(|binding| !binding.closed)
        .find(|binding| {
            s.resource_from_wire(&binding.agent)
                .is_none_or(|id| !cuts.contains_key(&id))
        })
        .map(|binding| binding.agent.clone())
}

/// Write `cuts` and the native controllers into `blob`. The controller
/// ledgers change with SDK traffic, not organization shape, so they are
/// captured here at the sealed cut, never in the tree's identity.
pub(crate) fn carry_cuts(
    s: &ServerState,
    blob: &mut StateBlob,
    cuts: &HashMap<ResourceId, AgentStreamCut>,
) {
    blob.agent_sessions = session_blobs(s, cuts, &live_pane_wires(blob));
    blob.native_agents = s.native_agents().bindings.values().cloned().collect();
}

/// One [`AgentSessionBlob`] per cut whose session is still registered
/// under a pane that crosses live, in wire-id order. A session that
/// closed, or whose parent stopped crossing, since its cut is dropped.
fn session_blobs(
    s: &ServerState,
    cuts: &HashMap<ResourceId, AgentStreamCut>,
    live_panes: &HashSet<u32>,
) -> Vec<AgentSessionBlob> {
    let mut blobs: Vec<AgentSessionBlob> = cuts
        .iter()
        .filter_map(|(&id, cut)| {
            let desc = s.registry().resource(id)?;
            let facet = desc.agent.as_ref()?;
            let parent_wire_id = s.resource_wire(desc.parent?)?;
            if !live_panes.contains(&parent_wire_id) {
                return None;
            }
            Some(AgentSessionBlob {
                wire_id: s.resource_wire(id)?,
                parent_wire_id,
                provider: facet.provider.clone(),
                native_id: facet.native_id.clone(),
                state: facet.state.clone(),
                base_seq: cut.base_seq,
                dropped: cut.dropped,
                ended: cut.ended,
                records: cut.records.clone(),
                key: Some(desc.key),
                native_publications: cut.native_publications.clone(),
                native_publication_high_water: cut.native_publication_high_water,
            })
        })
        .collect();
    blobs.sort_by_key(|blob| blob.wire_id);
    blobs
}

/// The `AgentSession`s a re-exec carries, each with its bootstrap mailbox:
/// every session whose parent is a pane the upgrade carries. A session under
/// a retained or uncarried pane is dropped, as its parent is.
pub(crate) fn carried_sessions(
    s: &ServerState,
) -> Vec<(ResourceId, mpsc::Sender<BootstrapRequest>)> {
    let parents: HashSet<ResourceId> = s.carried_panes().into_iter().map(|(id, _)| id).collect();
    let mut carried: Vec<(ResourceId, mpsc::Sender<BootstrapRequest>)> = s
        .resource_ids()
        .into_iter()
        .filter(|id| s.registry().resource(*id).is_some())
        .filter_map(|id| {
            let handle = s.resource_handle(id)?;
            let session = handle.agent_session().ok()?;
            handle
                .parent
                .filter(|parent| parents.contains(parent))
                .map(|_| (id, session.bootstrap.clone()))
        })
        .collect();
    carried.sort_by_key(|(id, _)| s.resource_wire(*id));
    carried
}

/// Cut one agent session's record stream for the upgrade, sealing it there
/// when `seal` (see [`BootstrapRequest::seal`]). `None` when the engine is
/// gone: the session is then simply not carried.
pub(crate) async fn request_agent_session_cut(
    bootstrap: &mpsc::Sender<BootstrapRequest>,
    seal: bool,
) -> Option<AgentSessionBootstrap> {
    let (reply, rx) = oneshot::channel();
    let request = BootstrapRequest {
        reply,
        seal: seal.then_some(true),
        after_seq: None,
    };
    bootstrap.send(request).await.ok()?;
    rx.await.ok()
}

/// An engine's cut, as the blob carries it.
pub(crate) fn stream_cut(cut: AgentSessionBootstrap) -> AgentStreamCut {
    AgentStreamCut {
        base_seq: cut.base_seq,
        dropped: cut.dropped,
        ended: cut.ended,
        // Stamped records are canonical JSON, so always UTF-8.
        records: cut
            .records
            .iter()
            .map(|record| String::from_utf8_lossy(record).into_owned())
            .collect(),
        native_publications: cut.native_publications,
        native_publication_high_water: cut.native_publication_high_water,
    }
}

/// The stream cut a resumed engine continues from, as the blob carried it.
fn carried_cut(carried: &AgentSessionBlob) -> AgentSessionBootstrap {
    AgentSessionBootstrap {
        base_seq: carried.base_seq,
        resume_after_seq: None,
        records: carried
            .records
            .iter()
            .map(|record| bytes::Bytes::from(record.clone()))
            .collect(),
        dropped: carried.dropped,
        ended: carried.ended,
        native_publications: carried.native_publications.clone(),
        native_publication_high_water: carried.native_publication_high_water,
    }
}

/// Rebuilds every carried session, then the native controllers.
#[derive(Debug, Default)]
pub(crate) struct AgentSessionResume;

impl KindRebuild for AgentSessionResume {
    /// Rebind every carried `AgentSession` to its record under its recorded
    /// wire id, with an engine that continues the old stream: same record
    /// counter, same retained tail, same `ended`. The parent's
    /// `REPORT_AGENT_STATE` is re-bound to the stream (ADR-0103 §6). A
    /// session whose parent pane or record did not cross is dropped.
    fn rebuild(&mut self, cx: &mut ResumeCx<'_>) -> Result<(), RebuildError> {
        let log_bytes = cx.state.agent_log_bytes();
        let blob = cx.blob;
        for carried in &blob.agent_sessions {
            let (Some(parent), Some(core)) = (
                cx.parent(carried.parent_wire_id),
                cx.record(carried.wire_id),
            ) else {
                continue;
            };
            if let Some(facet) = cx
                .state
                .registry_mut()
                .resource_mut(core)
                .and_then(|d| d.agent.as_mut())
            {
                facet.state.clone_from(&carried.state);
            }
            let token = CancellationToken::new();
            let bundle = AgentSessionActor::restore(
                parent,
                &carried.provider,
                carried.native_id.as_deref(),
                token.clone(),
                log_bytes,
                carried_cut(carried),
            );
            bind_to_parent(cx.state.resource_handle(parent), &bundle.handle);
            cx.spawn(
                core,
                carried.wire_id,
                bundle.handle,
                token,
                bundle.actor.run(),
                Some(bundle.exit_notify),
            )?;
        }
        Ok(())
    }

    fn restore(&mut self, state: &mut ServerState, blob: &StateBlob) {
        state.restore_native_agents(blob);
    }
}

impl KindResume for AgentSessionResume {
    fn start(&mut self, _state: &SharedState) -> Vec<(ResourceId, PaneEvents)> {
        Vec::new()
    }
}

/// Bind a rebuilt session's stream to its parent's agent reports.
fn bind_to_parent(parent: Option<&ResourceHandle>, session: &ResourceHandle) {
    let agent_parent =
        parent.and_then(|handle| handle.companion::<phux_agent_session::parent::AgentParent>());
    if let (Some(agent_parent), Ok(session)) = (agent_parent, session.agent_session()) {
        // The parent's mailbox is fresh, so this cannot be full.
        let _ = agent_parent.mailbox.try_send(
            phux_agent_session::parent::AgentParentRequest::BindSession {
                append: session.append.clone(),
            },
        );
    }
}

/// Agent-session streams sealed at their upgrade cut. Dropping the guard
/// unseals them: it drops only when the upgrade did not happen (a failed
/// preparation or `exec`), because a successful `exec` replaces the process.
#[derive(Debug, Default)]
pub(crate) struct AgentSessionSeal {
    /// Actors still owing rollback. Removing each completed actor immediately
    /// keeps cancellation from scheduling an extra unseal to that actor.
    pub(crate) bootstraps: Vec<mpsc::Sender<BootstrapRequest>>,
}

impl AgentSessionSeal {
    /// Attempt every unseal concurrently within one rollback window. An actor
    /// that acknowledges has applied the unseal before the next append; a
    /// closed actor owes nothing. Unresponsive actors remain for Drop, so a
    /// failed upgrade cannot block its caller or a healthy neighboring stream.
    pub(crate) async fn lift(&mut self) {
        if self.bootstraps.is_empty() {
            return;
        }
        let mut pending = self
            .bootstraps
            .iter()
            .cloned()
            .map(|bootstrap| async move {
                let (reply, applied) = oneshot::channel();
                if bootstrap
                    .send(BootstrapRequest {
                        reply,
                        seal: Some(false),
                        after_seq: None,
                    })
                    .await
                    .is_ok()
                {
                    // The actor replies after applying the seal. A dropped
                    // reply means its queued request died with the actor.
                    let _ = applied.await;
                }
                bootstrap
            })
            .collect::<FuturesUnordered<_>>();
        if tokio::time::timeout(AGENT_ROLLBACK_TIMEOUT, async {
            while let Some(completed) = pending.next().await {
                self.bootstraps
                    .retain(|bootstrap| !bootstrap.same_channel(&completed));
            }
        })
        .await
        .is_err()
        {
            tracing::warn!(
                pending = self.bootstraps.len(),
                "upgrade: agent rollback deadline elapsed; retaining unresolved streams for fallback"
            );
        }
    }
}

impl Drop for AgentSessionSeal {
    fn drop(&mut self) {
        for bootstrap in &self.bootstraps {
            let unseal = || BootstrapRequest {
                reply: oneshot::channel().0,
                seal: Some(false),
                after_seq: None,
            };
            if let Err(mpsc::error::TrySendError::Full(_)) = bootstrap.try_send(unseal()) {
                // A full mailbox must not leave the stream sealed for good.
                if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                    let bootstrap = bootstrap.clone();
                    drop(runtime.spawn(async move {
                        let _ = bootstrap.send(unseal()).await;
                    }));
                }
            }
        }
    }
}

/// Cut and seal every carried agent session's record stream, concurrently,
/// within `deadline`. The seal refuses appends from the cut until the
/// re-exec, so no record is acked and then lost. Unlike a pane, a session
/// that does not answer never aborts the upgrade: it holds no PTY to strand,
/// so it is dropped with a warning.
pub(crate) async fn collect_agent_session_cuts(
    sessions: Vec<(phux_core::ids::ResourceId, mpsc::Sender<BootstrapRequest>)>,
    deadline: Duration,
) -> (
    HashMap<phux_core::ids::ResourceId, AgentStreamCut>,
    AgentSessionSeal,
) {
    let wanted = sessions.len();
    let seal = AgentSessionSeal {
        bootstraps: sessions
            .iter()
            .map(|(_, bootstrap)| bootstrap.clone())
            .collect(),
    };
    let mut pending = sessions
        .into_iter()
        .map(|(session, bootstrap)| async move {
            request_agent_session_cut(&bootstrap, true)
                .await
                .map(|cut| (session, stream_cut(cut)))
        })
        .collect::<FuturesUnordered<_>>();
    let mut cuts = HashMap::with_capacity(wanted);
    let _ = tokio::time::timeout(deadline, async {
        while let Some(answer) = pending.next().await {
            if let Some((session, cut)) = answer {
                cuts.insert(session, cut);
            }
        }
    })
    .await;
    if cuts.len() < wanted {
        tracing::warn!(
            carried = cuts.len(),
            wanted,
            "upgrade: some agent sessions did not answer their cut; not carried"
        );
    }
    (cuts, seal)
}
