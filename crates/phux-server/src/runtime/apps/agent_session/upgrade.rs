//! Carrying agent sessions across a graceful upgrade (ADR-0032): sealing their
//! streams at the cut, rolling that back if the upgrade fails, and rebuilding
//! each engine in the resumed image. Dropping what [`begin`] returns unseals
//! the sessions; a successful re-exec replaces the process instead.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::FutureExt as _;
use futures_util::future::LocalBoxFuture;
use futures_util::stream::{FuturesUnordered, StreamExt as _};
use phux_core::ids::ResourceId;
use phux_resource::agent_session::AgentSessionResource as _;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::resource::ResourceHandle;
use crate::resource::agent_session::{AgentSessionActor, AgentSessionBootstrap, BootstrapRequest};
use crate::runtime::apps::StreamUpgrade;
use crate::state::upgrade_blob::RebuiltSession;
use crate::state::{ServerState, SharedState};
use crate::upgrade::blob::{AgentSessionBlob, AgentStreamCut};

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
    seal: AgentSessionSeal,
    _broker: Option<NativeBrokerSeal>,
}

/// Seal the native broker for an upgrade, or `None` when one already did.
pub(crate) fn begin(state: &SharedState) -> Option<Box<dyn StreamUpgrade>> {
    let broker = NativeBrokerSeal::new(state)?;
    Some(Box::new(AgentSessionUpgrade {
        seal: AgentSessionSeal::default(),
        _broker: Some(broker),
    }))
}

impl AgentSessionUpgrade {
    /// An upgrade already holding `seal`, with no broker behind it.
    #[cfg(test)]
    pub(crate) fn holding(seal: AgentSessionSeal) -> Box<dyn StreamUpgrade> {
        Box::new(Self {
            seal,
            _broker: None,
        })
    }
}

impl StreamUpgrade for AgentSessionUpgrade {
    fn cut<'a>(
        &'a mut self,
        state: &SharedState,
        deadline: Duration,
    ) -> LocalBoxFuture<'a, HashMap<ResourceId, AgentStreamCut>> {
        let sessions = state.with(carried_sessions);
        async move {
            let (cuts, seal) = collect_agent_session_cuts(sessions, deadline).await;
            self.seal = seal;
            cuts
        }
        .boxed_local()
    }

    fn uncarried(
        &self,
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

    fn lift(&mut self) -> LocalBoxFuture<'_, ()> {
        self.seal.lift().boxed_local()
    }
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

/// Rebuild one carried session and bind it to its parent, not yet running.
pub(crate) fn rebuild(
    carried: &AgentSessionBlob,
    parent: ResourceId,
    parent_handle: Option<&ResourceHandle>,
    log_bytes: u32,
    token: CancellationToken,
) -> RebuiltSession {
    let bundle = AgentSessionActor::restore(
        parent,
        &carried.provider,
        carried.native_id.as_deref(),
        token,
        log_bytes,
        carried_cut(carried),
    );
    if let (Some(parent_handle), Ok(session)) = (parent_handle, bundle.handle.agent_session()) {
        // The parent's mailbox is fresh, so this cannot be full.
        let _ = parent_handle
            .control
            .try_send(crate::resource::ControlRequest::BindAgentSession {
                append: session.append.clone(),
            });
    }
    RebuiltSession {
        handle: bundle.handle,
        engine: Box::pin(bundle.actor.run()),
        exit_notify: bundle.exit_notify,
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
