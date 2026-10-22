//! Collecting each pane's PTY handoff and sealing agent-session streams for
//! the upgrade cut (ADR-0032). Dropping the returned seal unseals the
//! sessions; a successful re-exec replaces the process instead.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt as _};
use tokio::sync::{mpsc, oneshot};

use crate::resource::agent_session::{AgentSessionBootstrap, BootstrapRequest};
use crate::state::request_agent_session_cut;
use crate::terminal_actor::{PaneUpgradeHandle, UpgradeHandleRequest};

use super::{AGENT_ROLLBACK_TIMEOUT, UpgradeError};

/// Ask one pane's actor for its handoff. Only live tree Terminals are asked
/// (see `ServerState::upgrade_handles`), so a closed upgrade mailbox means
/// a pane whose engine already exited and whose exit watcher has not reaped
/// it yet: [`UpgradeError::PaneExiting`], which a retry clears. Carrying it
/// would resurrect a killed pane as a blank one that never exits. Any
/// failure after the request was accepted (a dropped reply, a half PTY pair,
/// the deadline) means a live actor could not hand off.
pub(super) async fn request_pane_handoff(
    pane: phux_core::ids::ResourceId,
    upgrade: &mpsc::Sender<UpgradeHandleRequest>,
) -> Result<PaneUpgradeHandle, UpgradeError> {
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

pub(super) async fn collect_pane_handoffs(
    handles: Vec<(
        phux_core::ids::ResourceId,
        mpsc::Sender<UpgradeHandleRequest>,
    )>,
    deadline: Duration,
) -> Result<HashMap<phux_core::ids::ResourceId, PaneUpgradeHandle>, UpgradeError> {
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

/// Agent-session streams sealed at their upgrade cut. Dropping the guard
/// unseals them: it drops only when the upgrade did not happen (a failed
/// preparation or `exec`), because a successful `exec` replaces the process.
pub(super) struct AgentSessionSeal {
    /// Actors still owing rollback. Removing each completed actor immediately
    /// keeps cancellation from scheduling an extra unseal to that actor.
    pub(super) bootstraps: Vec<mpsc::Sender<BootstrapRequest>>,
}

impl AgentSessionSeal {
    /// Attempt every unseal concurrently within one rollback window. An actor
    /// that acknowledges has applied the unseal before the next append; a
    /// closed actor owes nothing. Unresponsive actors remain for Drop, so a
    /// failed upgrade cannot block its caller or a healthy neighboring stream.
    pub(super) async fn lift(&mut self) {
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
pub(super) async fn collect_agent_session_cuts(
    sessions: Vec<(phux_core::ids::ResourceId, mpsc::Sender<BootstrapRequest>)>,
    deadline: Duration,
) -> (
    HashMap<phux_core::ids::ResourceId, AgentSessionBootstrap>,
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
                .map(|cut| (session, cut))
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
